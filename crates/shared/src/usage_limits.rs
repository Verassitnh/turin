//! Port of upstream/packages/shared/src/usageLimits.ts
//!
//! Selection and pace maths for the provider limits view: which providers show,
//! what "ahead of pace" means, and how a reset is phrased.
//!
//! Provider fields are read through [`UsageProviderInfo`], so this crate does not depend on the
//! provider layer. Millisecond instants are `f64` to match JS `number` arithmetic; `Date.parse`
//! failures are `None` where upstream compares `NaN`.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, SecondsFormat, Utc};
use indexmap::IndexMap;
use turin_contracts::{
    EnvironmentId, TrimmedNonEmptyString,
    base_schemas::js_trim,
    provider_instance::{ProviderDriverKind, ProviderInstanceId},
    provider_usage_limits::{
        ProviderConsumeResetCreditInput, ServerProviderUsageLimits,
        ServerProviderUsageUnavailableReason, ServerProviderUsageWindow,
        ServerProviderUsageWindowKind, UsageLimitSourceAccount,
        UsageLimitSourceConsumeResetCreditInput, UsageLimitSourceSnapshot,
        UsageLimitSourceSnapshots, UsageLimitsReport, UsageLimitsReportAccount,
    },
};

use crate::js_string::locale_compare;
use crate::usage_format::js_math_round;

const MINUTE: f64 = 60_000.0;
const HOUR: f64 = 60.0 * MINUTE;
const DAY: f64 = 24.0 * HOUR;

pub const CHATGPT_USAGE_URL: &str = "https://chatgpt.com/#settings/Usage";

/// What the usage-limit views read from one provider instance. `ServerProvider` in
/// `turin-provider-core` is expected to implement it, and so does the test fake in this module, so
/// this crate does not depend on the provider layer.
pub trait UsageProviderInfo: Clone {
    /// A slash command in the provider's own type.
    type SlashCommand: Clone;

    fn instance_id(&self) -> &ProviderInstanceId;
    fn driver(&self) -> &ProviderDriverKind;
    fn enabled(&self) -> bool;
    fn installed(&self) -> bool;
    /// Upstream `isProviderAvailable`: an absent availability counts as available.
    fn is_available(&self) -> bool;
    fn display_name(&self) -> Option<&str>;
    fn accent_color(&self) -> Option<&str>;
    /// `auth.status === "authenticated"`.
    fn is_authenticated(&self) -> bool;
    fn auth_email(&self) -> Option<&str>;
    fn auth_label(&self) -> Option<&str>;
    fn auth_workspace_id(&self) -> Option<&str>;
    /// `auth.subscriptionSharing === true`.
    fn has_subscription_sharing(&self) -> bool;
    fn usage_limits(&self) -> Option<&ServerProviderUsageLimits>;

    /// The name of a command, to drop an earlier `usage-limits` entry.
    fn command_name(command: &Self::SlashCommand) -> &str;
    /// A command in the provider's own type with the given name and description.
    fn make_slash_command(name: &str, description: &str) -> Self::SlashCommand;
    fn slash_commands(&self) -> &[Self::SlashCommand];
    fn set_slash_commands(&mut self, commands: Vec<Self::SlashCommand>);
    /// Applies `map` to each workspace snapshot's commands, in place. A provider without
    /// snapshots is left alone.
    fn map_workspace_snapshot_commands<F>(&mut self, map: F)
    where
        F: FnMut(&[Self::SlashCommand]) -> Vec<Self::SlashCommand>;
}

/// `usesChatGptSharing`: verified sharing metadata, not the driver or the login type.
pub fn uses_chatgpt_sharing<P: UsageProviderInfo>(provider: Option<&P>) -> bool {
    provider
        .is_some_and(|provider| provider.is_authenticated() && provider.has_subscription_sharing())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorUsageWindow {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
}

pub static CURSOR_USAGE_WINDOWS: [CursorUsageWindow; 3] = [
    CursorUsageWindow {
        id: "totalPercentUsed",
        label: "Overall",
        description: "Combined usage across both allowances, not a third quota.",
    },
    CursorUsageWindow {
        id: "autoPercentUsed",
        label: "Cursor Models",
        description: "Grok and Composer use this first. Auto can use either pool.",
    },
    CursorUsageWindow {
        id: "apiPercentUsed",
        label: "Other Models",
        description: "Claude, GPT, and Gemini use this pool. Grok and Composer fall back here.",
    },
];

pub fn cursor_usage_window_details(id: &str) -> Option<&'static CursorUsageWindow> {
    CURSOR_USAGE_WINDOWS.iter().find(|window| window.id == id)
}

fn cursor_usage_window_rank(id: &str) -> usize {
    CURSOR_USAGE_WINDOWS
        .iter()
        .position(|window| window.id == id)
        .unwrap_or(CURSOR_USAGE_WINDOWS.len())
}

/// Providers that belong on the Limits view: enabled, installed, and one whose
/// driver reports subscription usage at all. A driver with no notion of usage
/// never sets `usageLimits`, so it has no row rather than an empty one.
pub fn providers_with_limits<P: UsageProviderInfo>(providers: &[P]) -> Vec<&P> {
    providers
        .iter()
        .filter(|provider| {
            provider.enabled()
                && provider.installed()
                && provider.is_available()
                && provider.usage_limits().is_some()
        })
        .collect()
}

/// What Limits needs from one connected environment: its label, its native
/// providers, and its usage-limit sources. A null `serverConfig` upstream is
/// the same as empty lists here.
#[derive(Debug, Clone)]
pub struct LimitPresentation<P> {
    pub label: String,
    pub providers: Vec<P>,
    pub usage_limit_sources: UsageLimitSourceSnapshots,
}

pub type LimitPresentations<P> = IndexMap<EnvironmentId, LimitPresentation<P>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalUsageLink {
    pub label: String,
    pub url: String,
    pub message: Option<String>,
    pub accounts: Vec<String>,
}

/// One destination per service, even when several accounts or environments use it.
pub fn collect_external_usage_links<P: UsageProviderInfo>(
    presentations: &LimitPresentations<P>,
) -> Vec<ExternalUsageLink> {
    let mut links: IndexMap<String, ExternalUsageLink> = IndexMap::new();
    for presentation in presentations.values() {
        for provider in providers_with_limits(&presentation.providers) {
            let Some(external) = provider
                .usage_limits()
                .and_then(|limits| limits.external_usage.as_ref())
            else {
                continue;
            };
            if !provider.is_authenticated() {
                continue;
            }
            let account = format!(
                "{} on {}",
                provider
                    .display_name()
                    .unwrap_or_else(|| provider.instance_id().as_str()),
                presentation.label
            );
            let url = external.url.as_str().to_owned();
            let mut accounts = links
                .get(&url)
                .map(|link| link.accounts.clone())
                .unwrap_or_default();
            if !accounts.contains(&account) {
                accounts.push(account);
            }
            let message = provider
                .usage_limits()
                .and_then(|limits| limits.unavailable.as_ref())
                .and_then(|unavailable| unavailable.message.as_ref())
                .map(|message| message.as_str().to_owned());
            links.insert(
                url.clone(),
                ExternalUsageLink {
                    label: external.label.as_str().to_owned(),
                    url,
                    message,
                    accounts,
                },
            );
        }
    }
    links.into_values().collect()
}

/// Prefer the reported email; use an identical credential when no email is available.
fn account_key(
    driver: &str,
    email: Option<&str>,
    limits: Option<&ServerProviderUsageLimits>,
) -> Option<String> {
    let normalized_email = email
        .map(|email| js_trim(email).to_lowercase())
        .filter(|email| !email.is_empty());
    if let Some(email) = normalized_email {
        return Some(format!("{driver}:{email}"));
    }
    limits
        .and_then(|limits| limits.credential_fingerprint.as_ref())
        .map(|fingerprint| format!("{driver}:credential:{}", fingerprint.as_str()))
}

#[derive(Debug, Clone, Copy)]
struct AccountReport<'a> {
    driver: &'a str,
    email: Option<&'a str>,
    workspace_id: Option<&'a str>,
    usage_limits: Option<&'a ServerProviderUsageLimits>,
}

fn native_report<P: UsageProviderInfo>(provider: &P) -> AccountReport<'_> {
    AccountReport {
        driver: provider.driver().as_str(),
        email: provider.auth_email(),
        workspace_id: provider.auth_workspace_id(),
        usage_limits: provider.usage_limits(),
    }
}

fn source_report(account: &UsageLimitSourceAccount) -> AccountReport<'_> {
    AccountReport {
        driver: account.driver.as_str(),
        email: account.email.as_ref().map(|email| email.as_str()),
        workspace_id: account
            .workspace_id
            .as_ref()
            .map(|workspace| workspace.as_str()),
        usage_limits: Some(&account.usage_limits),
    }
}

/// Keys each report by the account it draws on. One email can belong to
/// several workspaces, each with its own quota, so a report that names its
/// workspace is keyed by it too. One that does not (a hub, an older server)
/// joins the only workspace known for its email, and gets no key when there
/// are several, since it cannot say which one it read. Build it from every
/// enabled login, read or not, so an unreadable workspace still counts as one.
struct AccountKeyResolver {
    workspaces: HashMap<String, HashSet<String>>,
}

impl AccountKeyResolver {
    fn new<'a>(reports: impl IntoIterator<Item = AccountReport<'a>>) -> Self {
        let mut workspaces: HashMap<String, HashSet<String>> = HashMap::new();
        for report in reports {
            if let (Some(key), Some(workspace)) = (
                account_key(report.driver, report.email, report.usage_limits),
                report.workspace_id,
            ) {
                workspaces
                    .entry(key)
                    .or_default()
                    .insert(workspace.to_owned());
            }
        }
        Self { workspaces }
    }

    fn key(&self, report: AccountReport<'_>) -> Option<String> {
        let key = account_key(report.driver, report.email, report.usage_limits)?;
        if let Some(workspace) = report.workspace_id {
            return Some(format!("{key}:{workspace}"));
        }
        match self.workspaces.get(&key) {
            None => Some(key),
            Some(known) if known.len() == 1 => {
                let workspace = known.iter().next()?;
                Some(format!("{key}:{workspace}"))
            }
            Some(_) => None,
        }
    }
}

/// One subscription account as the pooled views see it, whichever way it was
/// reported. Matching emails or credentials across environments name one
/// account, unless they name different workspaces. Its quota is one bucket, so
/// counting it twice would misstate what is left.
#[derive(Debug, Clone, PartialEq)]
pub struct LimitAccount {
    pub key: String,
    pub driver: ProviderDriverKind,
    /// The instance's configured name, which is not sensitive; `None` for hub accounts.
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub plan: Option<String>,
    pub accent_color: Option<String>,
    /// Environments the account is signed in on; empty when only a hub reports it.
    pub environments: Vec<LimitEnvironment>,
    /// The hub that reported it, when no environment has it natively.
    pub source_label: Option<String>,
    /// Where the displayed reset credit can be redeemed.
    pub redeem: Option<LimitRedeem>,
    pub limits: ServerProviderUsageLimits,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimitEnvironment {
    pub environment_id: EnvironmentId,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LimitRedeem {
    pub environment_id: EnvironmentId,
    pub input: ProviderConsumeResetCreditInput,
}

/// Newest read wins, but a read with no parseable timestamp never does (JS `NaN > x` is false).
fn is_later(left: &str, right: &str) -> bool {
    js_date_diff(left, right) > 0.0
}

/// Accumulates accounts across environments, keeping the freshest read and
/// the redeem target that goes with it.
#[derive(Default)]
struct AccountMerger {
    accounts: IndexMap<String, LimitAccount>,
    credit_sources: HashMap<String, LimitAccount>,
    hub_redeems: HashMap<String, LimitAccount>,
}

impl AccountMerger {
    fn merge(&mut self, key: String, next: LimitAccount) {
        // Redeeming through a hub also clears the routing cooldown that hub holds
        // for the account. Redeeming natively against the same subscription resets
        // it upstream but leaves the hub refusing to route to the account until
        // its own cooldown expires, so a hub target wins the redemption outright
        // while the displayed balance still follows the freshest read.
        let is_hub_redeem = next.redeem.as_ref().is_some_and(|redeem| {
            matches!(redeem.input, ProviderConsumeResetCreditInput::Source(_))
        });
        if is_hub_redeem
            && self.hub_redeems.get(&key).is_none_or(|previous| {
                is_later(&next.limits.checked_at, &previous.limits.checked_at)
            })
        {
            self.hub_redeems.insert(key.clone(), next.clone());
        }
        if next.limits.reset_credits.is_some()
            && self.credit_sources.get(&key).is_none_or(|previous| {
                is_later(&next.limits.checked_at, &previous.limits.checked_at)
            })
        {
            self.credit_sources.insert(key.clone(), next.clone());
        }
        let Some(previous) = self.accounts.get(&key).cloned() else {
            self.accounts.insert(key, next);
            return;
        };
        let fresher = is_later(&next.limits.checked_at, &previous.limits.checked_at);
        // Two instances on one machine sharing an account still name it once.
        let mut environments = previous.environments.clone();
        environments.extend(
            next.environments
                .iter()
                .filter(|candidate| {
                    !previous
                        .environments
                        .iter()
                        .any(|seen| seen.environment_id == candidate.environment_id)
                })
                .cloned(),
        );
        let winner = if fresher { &next } else { &previous };
        // Credits and their redemption target travel together. A failed credit
        // probe must not erase a successful read from another environment.
        let credit_source = self.credit_sources.get(&key);
        let redeem = self
            .hub_redeems
            .get(&key)
            .and_then(|hub| hub.redeem.clone())
            .or_else(|| match credit_source {
                Some(source) => source.redeem.clone(),
                None => winner
                    .redeem
                    .clone()
                    .or_else(|| previous.redeem.clone())
                    .or_else(|| next.redeem.clone()),
            });
        let reset_credits = credit_source.and_then(|source| source.limits.reset_credits.clone());
        let mut limits = winner.limits.clone();
        limits.reset_credits = reset_credits;
        // A hub only names the account when no environment has it natively.
        let source_label = if environments.is_empty() {
            previous
                .source_label
                .clone()
                .or_else(|| next.source_label.clone())
        } else {
            None
        };
        let merged = LimitAccount {
            key: previous.key.clone(),
            driver: previous.driver.clone(),
            display_name: previous
                .display_name
                .clone()
                .or_else(|| next.display_name.clone()),
            email: previous.email.clone(),
            plan: previous.plan.clone().or_else(|| next.plan.clone()),
            accent_color: previous
                .accent_color
                .clone()
                .or_else(|| next.accent_color.clone()),
            environments,
            source_label,
            redeem,
            limits,
        };
        self.accounts.insert(key, merged);
    }
}

fn native_limit_account<P: UsageProviderInfo>(
    environment_id: &EnvironmentId,
    label: &str,
    provider: &P,
    limits: &ServerProviderUsageLimits,
) -> LimitAccount {
    LimitAccount {
        key: format!(
            "{}:{}",
            environment_id.as_str(),
            provider.instance_id().as_str()
        ),
        driver: provider.driver().clone(),
        display_name: non_blank(provider.display_name()),
        email: provider.auth_email().map(str::to_owned),
        plan: provider.auth_label().map(str::to_owned),
        accent_color: provider.accent_color().map(str::to_owned),
        environments: vec![LimitEnvironment {
            environment_id: environment_id.clone(),
            label: label.to_owned(),
        }],
        source_label: None,
        redeem: Some(LimitRedeem {
            environment_id: environment_id.clone(),
            input: ProviderConsumeResetCreditInput::Instance {
                instance_id: provider.instance_id().clone(),
            },
        }),
        limits: limits.clone(),
    }
}

/// `displayName?.trim() || null`.
fn non_blank(value: Option<&str>) -> Option<String> {
    value
        .map(js_trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Every account with usable windows across the connected environments, one
/// entry per distinct account. The freshest reads supply windows and credits;
/// native instances supply names and environment labels.
pub fn collect_limit_accounts<P: UsageProviderInfo>(
    presentations: &LimitPresentations<P>,
) -> Vec<LimitAccount> {
    let mut merger = AccountMerger::default();
    let resolve_key = AccountKeyResolver::new(presentations.values().flat_map(|presentation| {
        presentation
            .providers
            .iter()
            .filter(|provider| provider.enabled())
            .map(native_report)
            .chain(
                presentation
                    .usage_limit_sources
                    .iter()
                    .flat_map(|source| source.accounts.iter().map(source_report)),
            )
    }));
    for (environment_id, presentation) in presentations {
        for provider in providers_with_limits(&presentation.providers) {
            let Some(limits) = provider.usage_limits() else {
                continue;
            };
            if limits_notice(limits).is_some() {
                continue;
            }
            let key = resolve_key.key(native_report(provider)).unwrap_or_else(|| {
                format!(
                    "{}:{}",
                    environment_id.as_str(),
                    provider.instance_id().as_str()
                )
            });
            let account =
                native_limit_account(environment_id, &presentation.label, provider, limits);
            merger.merge(key, account);
        }
    }
    // Every hub account, including those a native instance also knows: the hub
    // may hold a fresher read of the same subscription, and the merge above
    // keeps the redeem target consistent with whichever snapshot wins.
    let label_environment = presentations.len() > 1;
    for (environment_id, presentation) in presentations {
        for source in &presentation.usage_limit_sources {
            let source_label = if label_environment {
                format!("{} · {}", presentation.label, source.label.as_str())
            } else {
                source.label.as_str().to_owned()
            };
            for account in &source.accounts {
                if limits_notice(&account.usage_limits).is_some() {
                    continue;
                }
                let key = format!(
                    "{}:{}{}",
                    source.id,
                    account.id.as_str(),
                    account
                        .workspace_id
                        .as_ref()
                        .map_or(String::new(), |workspace| format!(
                            ":{}",
                            workspace.as_str()
                        ))
                );
                let resolved = resolve_key
                    .key(source_report(account))
                    .unwrap_or_else(|| key.clone());
                let redeem = account
                    .usage_limits
                    .reset_credits
                    .as_ref()
                    .and_then(|credits| credits.next_credit_id.as_ref())
                    .map(|credit_id| LimitRedeem {
                        environment_id: environment_id.clone(),
                        input: ProviderConsumeResetCreditInput::Source(
                            UsageLimitSourceConsumeResetCreditInput {
                                source_id: source.id.clone(),
                                account_id: account.id.clone(),
                                credit_id: credit_id.clone(),
                            },
                        ),
                    });
                merger.merge(
                    resolved,
                    LimitAccount {
                        key,
                        driver: account.driver.clone(),
                        display_name: if account.email.is_some() {
                            None
                        } else {
                            Some(strip_json_suffix(account.id.as_str()).to_owned())
                        },
                        email: account
                            .email
                            .as_ref()
                            .map(|email| email.as_str().to_owned()),
                        plan: account.plan.as_ref().map(|plan| plan.as_str().to_owned()),
                        accent_color: None,
                        environments: Vec::new(),
                        source_label: Some(source_label.clone()),
                        redeem,
                        limits: account.usage_limits.clone(),
                    },
                );
            }
        }
    }
    merger.accounts.into_values().collect()
}

/// `id.replace(/\.json$/i, "")`.
fn strip_json_suffix(id: &str) -> &str {
    let bytes = id.as_bytes();
    if bytes.len() >= 5 && bytes[bytes.len() - 5..].eq_ignore_ascii_case(b".json") {
        &id[..id.len() - 5]
    } else {
        id
    }
}

/// What the pooled views cannot draw as a bar: a hub that failed to read, a
/// provider whose probe failed. Accounts that can never report (API keys)
/// are left out; there is nothing for the user to act on. The environment
/// is named only when more than one is connected.
pub fn collect_limit_notices<P: UsageProviderInfo>(
    presentations: &LimitPresentations<P>,
) -> Vec<String> {
    let label = |environment_label: &str, subject: &str| {
        if presentations.len() > 1 {
            format!("{environment_label} · {subject}")
        } else {
            subject.to_owned()
        }
    };
    let mut notices = Vec::new();
    for presentation in presentations.values() {
        let environment_label = presentation.label.as_str();
        for provider in providers_with_limits(&presentation.providers) {
            // An account that can never report (API key) is left out; one that
            // failed, or reported nothing at all, is worth a line.
            if unavailable_reason(provider.usage_limits())
                == Some(ServerProviderUsageUnavailableReason::Unsupported)
            {
                continue;
            }
            let notice = provider.usage_limits().and_then(limits_notice);
            let name = non_blank(provider.display_name())
                .unwrap_or_else(|| provider.driver().as_str().to_owned());
            if let Some(notice) = notice {
                notices.push(format!("{}: {notice}", label(environment_label, &name)));
            }
        }
        for source in &presentation.usage_limit_sources {
            let source_label = source.label.as_str();
            if let Some(error) = &source.error {
                notices.push(format!(
                    "{}: {}",
                    label(environment_label, source_label),
                    error.as_str()
                ));
            } else if source.accounts.is_empty() {
                notices.push(format!(
                    "{}: No accounts reported.",
                    label(environment_label, source_label)
                ));
            }
        }
    }
    notices
}

fn unavailable_reason(
    limits: Option<&ServerProviderUsageLimits>,
) -> Option<ServerProviderUsageUnavailableReason> {
    limits
        .and_then(|limits| limits.unavailable.as_ref())
        .map(|unavailable| unavailable.reason)
}

#[derive(Debug, Clone, PartialEq)]
pub struct LimitPoolMember {
    pub account: LimitAccount,
    pub window: ServerProviderUsageWindow,
}

/// One account column of a pooled window row. `window` is `None` where the
/// account does not report this window, which leaves a gap.
#[derive(Debug, Clone, PartialEq)]
pub struct LimitPoolColumn {
    pub account: LimitAccount,
    pub window: Option<ServerProviderUsageWindow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LimitPoolReset {
    pub member: LimitPoolMember,
    /// Epoch milliseconds of the reset.
    pub at: f64,
    /// Points of the pool the reset restores: the member's used share over the member count.
    pub restores_percent: f64,
}

/// One window id across every account that reports it: the pooled share left,
/// pace against the clock, and the resets in the order they will land, each
/// with the share of the pool it hands back.
#[derive(Debug, Clone, PartialEq)]
pub struct LimitPoolWindow {
    pub id: String,
    pub kind: ServerProviderUsageWindowKind,
    pub label: String,
    pub members: Vec<LimitPoolMember>,
    /// Fixed account positions across rows; a null window leaves a gap.
    pub columns: Vec<LimitPoolColumn>,
    pub remaining_percent: f64,
    pub used_percent: f64,
    pub pace: Option<LimitPace>,
    pub resets: Vec<LimitPoolReset>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LimitPool {
    pub driver: ProviderDriverKind,
    pub accounts: Vec<LimitAccount>,
    pub windows: Vec<LimitPoolWindow>,
}

/// Show Cursor's two usable pools instead of a combined percentage when both are available.
pub fn display_limit_windows(pool: &LimitPool) -> Vec<&LimitPoolWindow> {
    let mut windows: Vec<&LimitPoolWindow> = pool.windows.iter().collect();
    if pool.driver.as_str() == "antigravity" {
        // Keep each model group's five-hour and weekly buckets next to one another.
        windows.sort_by(|left, right| {
            (!left.id.starts_with("gemini-"))
                .cmp(&!right.id.starts_with("gemini-"))
                .then_with(|| {
                    locale_compare(
                        strip_antigravity_suffix(&left.id),
                        strip_antigravity_suffix(&right.id),
                    )
                })
                .then_with(|| window_kind_order(left.kind).cmp(&window_kind_order(right.kind)))
        });
        return windows;
    }
    if pool.driver.as_str() != "cursor" {
        return windows;
    }
    let has_auto = pool
        .windows
        .iter()
        .any(|window| window.id == "autoPercentUsed");
    let has_api = pool
        .windows
        .iter()
        .any(|window| window.id == "apiPercentUsed");
    let has_both_pools = has_auto && has_api;
    let mut windows: Vec<&LimitPoolWindow> = pool
        .windows
        .iter()
        .filter(|window| !has_both_pools || window.id != "totalPercentUsed")
        .collect();
    windows.sort_by_key(|window| cursor_usage_window_rank(&window.id));
    windows
}

fn strip_antigravity_suffix(id: &str) -> &str {
    for suffix in ["-5h", "-weekly", "-quota"] {
        if let Some(stripped) = id.strip_suffix(suffix) {
            return stripped;
        }
    }
    id
}

fn window_kind_order(kind: ServerProviderUsageWindowKind) -> u8 {
    match kind {
        ServerProviderUsageWindowKind::Session => 0,
        ServerProviderUsageWindowKind::Weekly => 1,
        ServerProviderUsageWindowKind::Monthly => 2,
        ServerProviderUsageWindowKind::Other => 3,
    }
}

fn window_kind_str(kind: ServerProviderUsageWindowKind) -> &'static str {
    match kind {
        ServerProviderUsageWindowKind::Session => "session",
        ServerProviderUsageWindowKind::Weekly => "weekly",
        ServerProviderUsageWindowKind::Monthly => "monthly",
        ServerProviderUsageWindowKind::Other => "other",
    }
}

/// `a - b` as a sort comparison. `NaN` (and `0`) compare equal, as JS does.
fn cmp_js_diff(diff: f64) -> std::cmp::Ordering {
    if diff.is_nan() || diff == 0.0 {
        std::cmp::Ordering::Equal
    } else if diff < 0.0 {
        std::cmp::Ordering::Less
    } else {
        std::cmp::Ordering::Greater
    }
}

/// Accounts grouped by driver, each with its windows pooled by kind and id.
/// Window ids are stable per provider, so a hub row and a native row for the
/// same window land in the same pool; the kind is part of the key because
/// Codex's `primary` is a position, not a duration (five hours on paid plans,
/// a month on Free/Go), and a monthly allowance must not average into a
/// five-hour pool. Pools order by kind, then first appearance.
///
/// Accounts and columns share the session reset order, soonest first. When
/// no account reports a session window, use the first window by kind instead.
/// Missing reset times sort last, with account names and keys breaking ties.
/// Each window's reset list still follows its own clock.
pub fn collect_limit_pools(accounts: &[LimitAccount], now: f64) -> Vec<LimitPool> {
    let mut by_driver: IndexMap<String, Vec<LimitAccount>> = IndexMap::new();
    for account in accounts {
        by_driver
            .entry(account.driver.as_str().to_owned())
            .or_default()
            .push(account.clone());
    }
    by_driver
        .into_values()
        .map(|members| {
            let driver = members[0].driver.clone();
            let driver_name = driver.as_str();
            let order_window = members
                .iter()
                .flat_map(|account| account.limits.windows.iter())
                .min_by_key(|window| window_kind_order(window.kind));
            let order_key = order_window.map(|window| pool_key(driver_name, window));
            let order_reset = |account: &LimitAccount| -> f64 {
                let Some(order_key) = order_key.as_deref() else {
                    return f64::INFINITY;
                };
                account
                    .limits
                    .windows
                    .iter()
                    .find(|window| pool_key(driver_name, window) == order_key)
                    .and_then(reset_millis)
                    .unwrap_or(f64::INFINITY)
            };
            let mut sorted = members;
            sorted.sort_by(|left, right| {
                cmp_js_diff(order_reset(left) - order_reset(right))
                    .then_with(|| {
                        locale_compare(&account_sort_name(left), &account_sort_name(right))
                    })
                    .then_with(|| locale_compare(&left.key, &right.key))
            });
            let windows = pool_windows(&sorted, now);
            LimitPool {
                driver,
                accounts: sorted,
                windows,
            }
        })
        .collect()
}

fn account_sort_name(account: &LimitAccount) -> String {
    account
        .display_name
        .as_deref()
        .or(account.email.as_deref())
        .unwrap_or(&account.key)
        .to_lowercase()
}

/// Codex ids are slots, not allowances: a weekly-only plan reports its weekly
/// window as `primary`, other plans as `secondary`. Its windows pool by kind;
/// other drivers' ids name distinct allowances.
fn pool_key(driver: &str, window: &ServerProviderUsageWindow) -> String {
    if driver == "codex" {
        window_kind_str(window.kind).to_owned()
    } else {
        format!("{}:{}", window_kind_str(window.kind), window.id.as_str())
    }
}

fn pool_windows(accounts: &[LimitAccount], now: f64) -> Vec<LimitPoolWindow> {
    let mut by_key: IndexMap<String, Vec<LimitPoolMember>> = IndexMap::new();
    for account in accounts {
        for window in &account.limits.windows {
            let pooled = pool_key(account.driver.as_str(), window);
            // An account with two windows of one kind keeps the second apart.
            let already_pooled = by_key.get(&pooled).is_some_and(|members| {
                members
                    .iter()
                    .any(|member| member.account.key == account.key)
            });
            let key = if already_pooled {
                format!("{}:{}", window_kind_str(window.kind), window.id.as_str())
            } else {
                pooled
            };
            by_key.entry(key).or_default().push(LimitPoolMember {
                account: account.clone(),
                window: window.clone(),
            });
        }
    }
    let mut pools: Vec<LimitPoolWindow> = by_key
        .into_iter()
        .map(|(key, members)| {
            let member_by_account: HashMap<&str, &LimitPoolMember> = members
                .iter()
                .map(|member| (member.account.key.as_str(), member))
                .collect();
            let first = members[0].window.clone();
            // Pooled Codex slots take the pool's own key, so the id stays the same
            // whichever account sorts first.
            let id = if members[0].account.driver.as_str() == "codex" {
                key
            } else {
                first.id.as_str().to_owned()
            };
            let count = members.len() as f64;
            let used_percent = members
                .iter()
                .map(|member| member.window.used_percent)
                .sum::<f64>()
                / count;
            // Pace compares spend against the clock, so it is judged only over the
            // members that have a clock; a window with no reset would otherwise
            // count as spend with no time elapsed and skew the verdict.
            let timed: Vec<(f64, f64)> = members
                .iter()
                .filter_map(|member| {
                    elapsed_share(&member.window, now)
                        .map(|elapsed| (member.window.used_percent, elapsed))
                })
                .collect();
            let timed_used = timed.iter().map(|(used, _)| used).sum::<f64>() / timed.len() as f64;
            let mean_elapsed = (!timed.is_empty()).then(|| {
                timed.iter().map(|(_, elapsed)| elapsed).sum::<f64>() / timed.len() as f64
            });
            let mut resets: Vec<LimitPoolReset> = members
                .iter()
                .filter_map(|member| {
                    reset_millis(&member.window).map(|at| LimitPoolReset {
                        member: member.clone(),
                        at,
                        restores_percent: js_math_round(member.window.used_percent / count),
                    })
                })
                .collect();
            resets.sort_by(|left, right| {
                left.at
                    .partial_cmp(&right.at)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let columns = accounts
                .iter()
                .map(
                    |account| match member_by_account.get(account.key.as_str()) {
                        Some(member) => LimitPoolColumn {
                            account: member.account.clone(),
                            window: Some(member.window.clone()),
                        },
                        None => LimitPoolColumn {
                            account: account.clone(),
                            window: None,
                        },
                    },
                )
                .collect();
            LimitPoolWindow {
                id,
                kind: first.kind,
                label: first.label.as_str().to_owned(),
                members,
                columns,
                used_percent: js_math_round(used_percent),
                remaining_percent: js_math_round(100.0 - used_percent),
                pace: mean_elapsed.map(|elapsed| pace_of_shares(timed_used, elapsed)),
                resets,
            }
        })
        .collect();
    pools.sort_by_key(|pool| window_kind_order(pool.kind));
    pools
}

/// The one-line status under a provider heading when there are no bars to draw.
pub fn limits_notice(limits: &ServerProviderUsageLimits) -> Option<String> {
    match &limits.unavailable {
        Some(unavailable) => match unavailable.reason {
            ServerProviderUsageUnavailableReason::Unsupported => {
                Some(unavailable.message.as_ref().map_or_else(
                    || "This account has no subscription limits.".to_owned(),
                    |message| message.as_str().to_owned(),
                ))
            }
            ServerProviderUsageUnavailableReason::ProbeFailed => {
                Some(unavailable.message.as_ref().map_or_else(
                    || "Could not read limits.".to_owned(),
                    |message| message.as_str().to_owned(),
                ))
            }
        },
        None if limits.windows.is_empty() => Some("No limits reported.".to_owned()),
        None => None,
    }
}

/// Quota left in the window, 0..100. Bars and labels show what remains, as Codex does.
pub fn remaining_percent(window: &ServerProviderUsageWindow) -> f64 {
    js_math_round(100.0 - window.used_percent.clamp(0.0, 100.0))
}

/// `Date.parse` for the ISO-8601 instants this crate reads; `None` where JS gives `NaN`.
pub(crate) fn parse_date_ms(value: &str) -> Option<f64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|instant| instant.timestamp_millis() as f64)
}

/// `Date.parse(left) - Date.parse(right)`, `NaN` when either side does not parse.
fn js_date_diff(left: &str, right: &str) -> f64 {
    match (parse_date_ms(left), parse_date_ms(right)) {
        (Some(left), Some(right)) => left - right,
        _ => f64::NAN,
    }
}

fn reset_millis(window: &ServerProviderUsageWindow) -> Option<f64> {
    window.resets_at.as_deref().and_then(parse_date_ms)
}

/// Elapsed share of the window, 0..1, or `None` when its length or reset is unknown.
pub fn elapsed_share(window: &ServerProviderUsageWindow, now: f64) -> Option<f64> {
    let resets_at = reset_millis(window)?;
    let length = window.window_duration_mins? as f64 * MINUTE;
    if length <= 0.0 {
        return None;
    }
    Some(((length - (resets_at - now)) / length).clamp(0.0, 1.0))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitPace {
    Ahead,
    On,
    Under,
}

impl LimitPace {
    pub fn as_str(self) -> &'static str {
        match self {
            LimitPace::Ahead => "ahead",
            LimitPace::On => "on",
            LimitPace::Under => "under",
        }
    }
}

/// Usage against the clock. Spending evenly leaves the same share of quota as
/// there is time left in the window; within five points of that counts as on
/// pace, further ahead means the window may run dry first.
pub fn pace_of(window: &ServerProviderUsageWindow, now: f64) -> Option<LimitPace> {
    let elapsed = elapsed_share(window, now)?;
    Some(pace_of_shares(window.used_percent, elapsed))
}

fn pace_of_shares(used_percent: f64, elapsed: f64) -> LimitPace {
    let gap = used_percent - elapsed * 100.0;
    if gap > 5.0 {
        LimitPace::Ahead
    } else if gap < -5.0 {
        LimitPace::Under
    } else {
        LimitPace::On
    }
}

/// `2h 13m`, `3d 4h`, `12m`.
pub fn format_duration(ms: f64) -> String {
    let remaining = ms.max(0.0);
    let days = (remaining / DAY).floor();
    let hours = ((remaining % DAY) / HOUR).floor();
    let minutes = ((remaining % HOUR) / MINUTE).floor();
    if days > 0.0 {
        format!("{days}d {hours}h")
    } else if hours > 0.0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

/// `resets in 2h 13m`, or `None` when the window has no reset.
pub fn format_resets_in(window: &ServerProviderUsageWindow, now: f64) -> Option<String> {
    let resets_at = reset_millis(window)?;
    if resets_at <= now {
        return Some("resets now".to_owned());
    }
    if resets_at - now < MINUTE {
        return Some("Resets momentarily".to_owned());
    }
    Some(format!("resets in {}", format_duration(resets_at - now)))
}

pub const USAGE_LIMITS_COMMAND_NAME: &str = "usage-limits";

/// Description of the `usage-limits` command: Limit commands are served by T3 from the same
/// snapshots as Usage → Limits.
pub const USAGE_LIMITS_COMMAND_DESCRIPTION: &str = "Show this provider's usage limits";

/// Handled by the client without sending a turn; anything with arguments stays an ordinary prompt.
pub fn is_usage_limits_command(prompt: &str) -> bool {
    js_trim(prompt).to_lowercase() == "/usage-limits"
}

/// Whether Limits has anything to say about this driver. A source that failed to
/// read keeps no accounts, so its error counts for every driver rather than
/// disappearing until the next successful refresh.
pub fn has_provider_usage_limits<P: UsageProviderInfo>(
    driver: &str,
    providers: &[P],
    sources: &[UsageLimitSourceSnapshot],
) -> bool {
    providers_with_limits(providers)
        .iter()
        .any(|provider| provider.driver().as_str() == driver)
        || sources.iter().any(|source| {
            source
                .accounts
                .iter()
                .any(|account| account.driver.as_str() == driver)
                || (source.error.is_some() && source.accounts.is_empty())
        })
}

/// The drivers a set of sources would offer the command to, where a source that
/// failed to read counts for every driver. Two snapshots with the same coverage
/// need no catalog republish, however much their quotas moved.
pub fn same_usage_limit_command_coverage(
    previous: &[UsageLimitSourceSnapshot],
    next: &[UsageLimitSourceSnapshot],
) -> bool {
    let coverage = |sources: &[UsageLimitSourceSnapshot]| -> HashSet<String> {
        sources
            .iter()
            .flat_map(|source| {
                if source.error.is_some() && source.accounts.is_empty() {
                    vec!["*".to_owned()]
                } else {
                    source
                        .accounts
                        .iter()
                        .map(|account| account.driver.as_str().to_owned())
                        .collect()
                }
            })
            .collect()
    };
    let before = coverage(previous);
    let after = coverage(next);
    before.len() == after.len() && before.iter().all(|driver| after.contains(driver))
}

/// Advertise on workspace catalogs too, which replace the global command list.
pub fn with_usage_limits_commands<P: UsageProviderInfo>(
    providers: &[P],
    sources: &[UsageLimitSourceSnapshot],
) -> Vec<P> {
    let with_command = |items: &[P::SlashCommand]| -> Vec<P::SlashCommand> {
        items
            .iter()
            .filter(|command| P::command_name(command) != USAGE_LIMITS_COMMAND_NAME)
            .cloned()
            .chain(std::iter::once(P::make_slash_command(
                USAGE_LIMITS_COMMAND_NAME,
                USAGE_LIMITS_COMMAND_DESCRIPTION,
            )))
            .collect()
    };
    providers
        .iter()
        .map(|provider| {
            if !has_provider_usage_limits(provider.driver().as_str(), providers, sources) {
                return provider.clone();
            }
            let mut next = provider.clone();
            next.set_slash_commands(with_command(provider.slash_commands()));
            next.map_workspace_snapshot_commands(|snapshot: &[P::SlashCommand]| {
                with_command(snapshot)
            });
            next
        })
        .collect()
}

/// A point-in-time report; never refreshes or guesses which pooled account serves a turn.
pub fn collect_provider_usage_limits<P: UsageProviderInfo>(
    instance_id: &ProviderInstanceId,
    providers: &[P],
    sources: &[UsageLimitSourceSnapshot],
    now: f64,
) -> Option<UsageLimitsReport> {
    let selected = providers
        .iter()
        .find(|provider| provider.instance_id() == instance_id)?;
    if !has_provider_usage_limits(selected.driver().as_str(), providers, sources) {
        return None;
    }
    let native: Vec<&P> = providers_with_limits(providers)
        .into_iter()
        .filter(|provider| provider.driver() == selected.driver())
        .collect();
    let resolve_key = AccountKeyResolver::new(
        providers
            .iter()
            .filter(|provider| provider.enabled())
            .map(native_report)
            .chain(
                sources
                    .iter()
                    .flat_map(|source| source.accounts.iter().map(source_report)),
            ),
    );
    let native_accounts: HashSet<String> = native
        .iter()
        .filter_map(|provider| {
            let key = resolve_key.key(native_report(*provider))?;
            let limits = provider.usage_limits()?;
            (!limits.windows.is_empty() && limits.unavailable.is_none()).then_some(key)
        })
        .collect();
    let mut accounts: Vec<UsageLimitsReportAccount> = Vec::new();
    let mut notices: Vec<String> = Vec::new();
    for provider in &native {
        let Some(limits) = provider.usage_limits() else {
            continue;
        };
        let key = resolve_key.key(native_report(*provider));
        // Two independent decisions. Which balance to *display* follows whichever
        // snapshot is fresher. Which path to *redeem through* always prefers the
        // hub, because only the hub path clears the routing cooldown it holds for
        // that account; redeeming natively against the same account resets the
        // subscription upstream but leaves the hub refusing to route to it until
        // its own cooldown expires. A hub credit id that a fresher native redeem
        // already spent comes back as `alreadyRedeemed`, which still clears the
        // cooldown, so preferring it is safe even when the hub snapshot is stale.
        let hub_credits = key.as_ref().and_then(|key| {
            let mut candidates: Vec<(&UsageLimitSourceSnapshot, &UsageLimitSourceAccount)> =
                sources
                    .iter()
                    .flat_map(|source| source.accounts.iter().map(move |account| (source, account)))
                    .filter(|(_, account)| {
                        account.usage_limits.reset_credits.is_some()
                            && limits_notice(&account.usage_limits).is_none()
                            && resolve_key.key(source_report(account)).as_deref()
                                == Some(key.as_str())
                    })
                    .collect();
            candidates.sort_by(|(_, a), (_, b)| {
                cmp_js_diff(js_date_diff(
                    &b.usage_limits.checked_at,
                    &a.usage_limits.checked_at,
                ))
            });
            candidates.into_iter().next()
        });
        let hub_credit_id = hub_credits.and_then(|(_, account)| {
            account
                .usage_limits
                .reset_credits
                .as_ref()
                .and_then(|credits| credits.next_credit_id.clone())
        });
        let show_hub_credits = hub_credits.is_some_and(|(_, account)| {
            limits.reset_credits.is_none()
                || is_later(&account.usage_limits.checked_at, &limits.checked_at)
        });
        let display_name = non_blank(provider.display_name())
            .unwrap_or_else(|| provider.driver().as_str().to_owned());
        accounts.push(UsageLimitsReportAccount {
            id: tne(provider.instance_id().as_str()),
            driver: provider.driver().clone(),
            label: tne(format!(
                "{display_name} [{}]",
                provider.instance_id().as_str()
            )),
            plan: provider.auth_label().map(tne),
            instance_id: Some(provider.instance_id().clone()),
            reset_credit_input: match (hub_credit_id, hub_credits) {
                (Some(credit_id), Some((source, account))) => {
                    Some(ProviderConsumeResetCreditInput::Source(
                        UsageLimitSourceConsumeResetCreditInput {
                            source_id: source.id.clone(),
                            account_id: account.id.clone(),
                            credit_id,
                        },
                    ))
                }
                _ => Some(ProviderConsumeResetCreditInput::Instance {
                    instance_id: provider.instance_id().clone(),
                }),
            },
            display_name: provider.display_name().map(str::to_owned),
            accent_color: provider.accent_color().map(str::to_owned),
            email: provider.auth_email().map(tne),
            source_label: None,
            limits: match hub_credits {
                Some((_, account)) if show_hub_credits => {
                    let mut limits = limits.clone();
                    limits.reset_credits = account.usage_limits.reset_credits.clone();
                    limits
                }
                _ => limits.clone(),
            },
        });
    }
    for source in sources {
        let matching: Vec<&UsageLimitSourceAccount> = source
            .accounts
            .iter()
            .filter(|account| account.driver == *selected.driver())
            .collect();
        for account in &matching {
            let key = resolve_key.key(source_report(account));
            if key.is_some_and(|key| native_accounts.contains(&key)) {
                continue;
            }
            accounts.push(UsageLimitsReportAccount {
                id: tne(format!("{}:{}", source.id, account.id.as_str())),
                driver: account.driver.clone(),
                label: tne(format!(
                    "{} · {}",
                    source.label.as_str(),
                    account.id.as_str()
                )),
                plan: account.plan.clone(),
                email: account.email.clone(),
                source_label: Some(tne("CLI Proxy")),
                instance_id: None,
                reset_credit_input: account
                    .usage_limits
                    .reset_credits
                    .as_ref()
                    .and_then(|credits| credits.next_credit_id.as_ref())
                    .map(|credit_id| {
                        ProviderConsumeResetCreditInput::Source(
                            UsageLimitSourceConsumeResetCreditInput {
                                source_id: source.id.clone(),
                                account_id: account.id.clone(),
                                credit_id: credit_id.clone(),
                            },
                        )
                    }),
                display_name: None,
                accent_color: None,
                limits: account.usage_limits.clone(),
            });
        }
        // A source that failed to read has no accounts left to match on, so its
        // error is reported to every provider rather than silently dropped.
        if let Some(error) = source
            .error
            .as_ref()
            .filter(|_| !matching.is_empty() || source.accounts.is_empty())
        {
            notices.push(format!("{}: {}", source.label.as_str(), error.as_str()));
        }
    }
    Some(UsageLimitsReport {
        created_at: format_iso_millis(now),
        accounts,
        notices,
    })
}

/// `DateTime.formatIso(DateTime.makeUnsafe(ms))`.
fn format_iso_millis(ms: f64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms as i64)
        .map(|instant| instant.to_rfc3339_opts(SecondsFormat::Millis, true))
        .unwrap_or_default()
}

/// Labels built here are never blank, so the trim cannot fail.
fn tne(value: impl AsRef<str>) -> TrimmedNonEmptyString {
    TrimmedNonEmptyString::new(value).expect("usage label is never blank")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use serde_json::{Value, json};
    use turin_contracts::usage_limit_source_id::UsageLimitSourceId;

    /// Test stand-in for `ServerProvider`: the same wire keys, so fixtures stay JSON.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct FakeProvider {
        instance_id: ProviderInstanceId,
        driver: ProviderDriverKind,
        enabled: bool,
        installed: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        availability: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        accent_color: Option<String>,
        auth: FakeAuth,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage_limits: Option<ServerProviderUsageLimits>,
        #[serde(default)]
        slash_commands: Vec<FakeCommand>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_snapshots: Option<Vec<FakeSnapshot>>,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct FakeAuth {
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        email: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subscription_sharing: Option<bool>,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct FakeCommand {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct FakeSnapshot {
        #[serde(default)]
        slash_commands: Vec<FakeCommand>,
    }

    impl UsageProviderInfo for FakeProvider {
        type SlashCommand = FakeCommand;

        fn instance_id(&self) -> &ProviderInstanceId {
            &self.instance_id
        }
        fn driver(&self) -> &ProviderDriverKind {
            &self.driver
        }
        fn enabled(&self) -> bool {
            self.enabled
        }
        fn installed(&self) -> bool {
            self.installed
        }
        fn is_available(&self) -> bool {
            self.availability.as_deref() != Some("unavailable")
        }
        fn display_name(&self) -> Option<&str> {
            self.display_name.as_deref()
        }
        fn accent_color(&self) -> Option<&str> {
            self.accent_color.as_deref()
        }
        fn is_authenticated(&self) -> bool {
            self.auth.status == "authenticated"
        }
        fn auth_email(&self) -> Option<&str> {
            self.auth.email.as_deref()
        }
        fn auth_label(&self) -> Option<&str> {
            self.auth.label.as_deref()
        }
        fn auth_workspace_id(&self) -> Option<&str> {
            self.auth.workspace_id.as_deref()
        }
        fn has_subscription_sharing(&self) -> bool {
            self.auth.subscription_sharing == Some(true)
        }
        fn usage_limits(&self) -> Option<&ServerProviderUsageLimits> {
            self.usage_limits.as_ref()
        }
        fn command_name(command: &FakeCommand) -> &str {
            &command.name
        }
        fn make_slash_command(name: &str, description: &str) -> FakeCommand {
            FakeCommand {
                name: name.to_owned(),
                description: Some(description.to_owned()),
            }
        }
        fn slash_commands(&self) -> &[FakeCommand] {
            &self.slash_commands
        }
        fn set_slash_commands(&mut self, commands: Vec<FakeCommand>) {
            self.slash_commands = commands;
        }
        fn map_workspace_snapshot_commands<F>(&mut self, mut map: F)
        where
            F: FnMut(&[FakeCommand]) -> Vec<FakeCommand>,
        {
            for snapshot in self.workspace_snapshots.iter_mut().flatten() {
                snapshot.slash_commands = map(&snapshot.slash_commands);
            }
        }
    }

    type ServerProvider = FakeProvider;

    fn ms(value: &str) -> f64 {
        parse_date_ms(value).expect("test instant parses")
    }

    fn now() -> f64 {
        ms("2026-09-03T12:00:00.000Z")
    }

    /// Shallow merge, as a JS object spread (`{...base, ...overrides}`).
    fn merge(base: Value, overrides: Value) -> Value {
        let mut merged = base;
        for (key, value) in overrides.as_object().expect("overrides object") {
            merged[key] = value.clone();
        }
        merged
    }

    fn provider(overrides: Value) -> ServerProvider {
        let base = json!({
            "instanceId": "codex",
            "driver": "codex",
            "enabled": true,
            "installed": true,
            "version": null,
            "status": "ready",
            "auth": {"status": "authenticated"},
            "checkedAt": "2026-09-03T11:00:00.000Z",
            "models": [],
            "slashCommands": [],
            "skills": [],
        });
        serde_json::from_value(merge(base, overrides)).expect("provider fixture decodes")
    }

    /// `{...provider, ...overrides}` on an already-built provider.
    fn respread(base: &ServerProvider, overrides: Value) -> ServerProvider {
        let value = serde_json::to_value(base).expect("provider encodes");
        serde_json::from_value(merge(value, overrides)).expect("provider fixture decodes")
    }

    fn window(overrides: Value) -> ServerProviderUsageWindow {
        let base = json!({
            "id": "five_hour",
            "kind": "session",
            "label": "Session",
            "usedPercent": 40,
            "windowDurationMins": 300,
            "resetsAt": "2026-09-03T14:00:00.000Z",
        });
        serde_json::from_value(merge(base, overrides)).expect("window fixture decodes")
    }

    fn limits(overrides: Value) -> ServerProviderUsageLimits {
        let base = json!({"checkedAt": "2026-09-03T11:00:00.000Z", "windows": [window_json()]});
        serde_json::from_value(merge(base, overrides)).expect("limits fixture decodes")
    }

    fn window_json() -> Value {
        serde_json::to_value(window(json!({}))).expect("window encodes")
    }

    fn source(overrides: Value) -> UsageLimitSourceSnapshot {
        let base = json!({
            "id": "hub",
            "kind": "cliproxy",
            "label": "hub",
            "checkedAt": "2026-09-03T11:00:00.000Z",
            "accounts": [],
        });
        serde_json::from_value(merge(base, overrides)).expect("source fixture decodes")
    }

    fn env(id: &str) -> EnvironmentId {
        EnvironmentId::new(id).expect("environment id")
    }

    fn pres(
        label: &str,
        providers: Vec<ServerProvider>,
        sources: Vec<UsageLimitSourceSnapshot>,
    ) -> LimitPresentation<FakeProvider> {
        LimitPresentation {
            label: label.to_owned(),
            providers,
            usage_limit_sources: sources,
        }
    }

    fn map(
        entries: Vec<(&str, LimitPresentation<FakeProvider>)>,
    ) -> LimitPresentations<FakeProvider> {
        entries
            .into_iter()
            .map(|(id, presentation)| (env(id), presentation))
            .collect()
    }

    fn keys(accounts: &[LimitAccount]) -> Vec<&str> {
        accounts
            .iter()
            .map(|account| account.key.as_str())
            .collect()
    }

    fn driver(name: &str) -> ProviderDriverKind {
        ProviderDriverKind::new(name).expect("driver slug")
    }

    fn instance(name: &str) -> ProviderInstanceId {
        ProviderInstanceId::new(name).expect("instance slug")
    }

    fn env_label(account: &LimitAccount) -> Vec<(&str, &str)> {
        account
            .environments
            .iter()
            .map(|environment| {
                (
                    environment.environment_id.as_str(),
                    environment.label.as_str(),
                )
            })
            .collect()
    }

    #[test]
    fn pace_formats_the_reset_boundary() {
        let cases = [
            ("2026-09-03T12:01:00.000Z", "resets in 1m"),
            ("2026-09-03T12:00:59.999Z", "Resets momentarily"),
            ("2026-09-03T12:00:00.001Z", "Resets momentarily"),
            ("2026-09-03T12:00:00.000Z", "resets now"),
            ("2026-09-03T11:59:59.999Z", "resets now"),
        ];
        for (resets_at, label) in cases {
            let w = window(json!({"resetsAt": resets_at}));
            assert_eq!(
                format_resets_in(&w, now()).as_deref(),
                Some(label),
                "{resets_at}"
            );
        }
    }

    #[test]
    fn pace_ignores_invalid_reset_timestamps() {
        let w = window(json!({"resetsAt": "invalid"}));
        assert_eq!(format_resets_in(&w, now()), None);
    }

    #[test]
    fn pace_places_the_clock_three_fifths_through_a_five_hour_window() {
        let w = window(json!({}));
        assert!((elapsed_share(&w, now()).unwrap() - 0.6).abs() < 1e-9);
        assert_eq!(pace_of(&w, now()), Some(LimitPace::Under));
        assert_eq!(
            pace_of(&window(json!({"usedPercent": 62})), now()),
            Some(LimitPace::On)
        );
        assert_eq!(
            pace_of(&window(json!({"usedPercent": 80})), now()),
            Some(LimitPace::Ahead)
        );
    }

    #[test]
    fn pace_needs_a_reset_and_a_duration() {
        let no_reset = window(json!({"resetsAt": null}));
        assert_eq!(pace_of(&no_reset, now()), None);
        assert_eq!(format_resets_in(&no_reset, now()), None);
        let no_duration = serde_json::from_value::<ServerProviderUsageWindow>(json!({
            "id": "x", "kind": "session", "label": "L", "usedPercent": 40,
            "resetsAt": "2026-09-03T14:00:00.000Z",
        }))
        .unwrap();
        assert_eq!(pace_of(&no_duration, now()), None);
    }

    #[test]
    fn pace_phrases_the_reset_as_a_countdown() {
        let w = window(json!({}));
        assert_eq!(
            format_resets_in(&w, now()).as_deref(),
            Some("resets in 2h 0m")
        );
        let later = window(json!({"resetsAt": "2026-09-06T15:30:00.000Z"}));
        assert_eq!(
            format_resets_in(&later, now()).as_deref(),
            Some("resets in 3d 3h")
        );
        let past = window(json!({"resetsAt": "2026-09-03T11:00:00.000Z"}));
        assert_eq!(
            format_resets_in(&past, now()).as_deref(),
            Some("resets now")
        );
    }

    #[test]
    fn limits_notice_explains_empty_bars_and_passes_provider_messages_through() {
        assert_eq!(limits_notice(&limits(json!({}))), None);
        assert_eq!(
            limits_notice(&limits(json!({"windows": []}))).as_deref(),
            Some("No limits reported.")
        );
        assert_eq!(
            limits_notice(&limits(
                json!({"windows": [], "unavailable": {"reason": "unsupported"}})
            ))
            .as_deref(),
            Some("This account has no subscription limits.")
        );
        assert_eq!(
            limits_notice(&limits(json!({
                "windows": [],
                "unavailable": {"reason": "probeFailed", "message": "Codex timed out."},
            })))
            .as_deref(),
            Some("Codex timed out.")
        );
    }

    #[test]
    fn providers_with_limits_keeps_only_usable_providers_whose_driver_reports_limits() {
        let usage = json!({"checkedAt": "2026-09-03T11:00:00.000Z", "windows": [window_json()]});
        let codex = provider(json!({"usageLimits": usage}));
        let providers = vec![
            codex.clone(),
            provider(json!({"instanceId": "cursor", "driver": "cursor"})),
            provider(json!({"instanceId": "off", "enabled": false, "usageLimits": usage})),
            provider(json!({"instanceId": "gone", "installed": false, "usageLimits": usage})),
            provider(
                json!({"instanceId": "shadow", "availability": "unavailable", "usageLimits": usage}),
            ),
        ];
        let kept = providers_with_limits(&providers);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0], &codex);
    }

    fn laptop(providers: Vec<ServerProvider>) -> LimitPresentation<FakeProvider> {
        pres("Laptop", providers, vec![])
    }

    fn checked(overrides: Value) -> Value {
        merge(
            json!({"checkedAt": "2026-09-03T11:00:00.000Z", "windows": [window_json()]}),
            overrides,
        )
    }

    fn weekly_json() -> Value {
        json!({
            "id": "seven_day",
            "kind": "weekly",
            "label": "Weekly",
            "usedPercent": 40,
            "windowDurationMins": 7 * 24 * 60,
            "resetsAt": "2026-09-06T12:00:00.000Z",
        })
    }

    #[test]
    fn merges_one_account_reported_natively_on_two_environments_and_by_a_hub() {
        let native = provider(json!({
            "driver": "claudeAgent",
            "instanceId": "claude",
            "auth": {"status": "authenticated", "email": "Same@example.com"},
            "usageLimits": checked(json!({"windows": [window_json()]})),
        }));
        let desktop_native = respread(
            &native,
            json!({"usageLimits": checked(json!({
                "checkedAt": "2026-09-03T11:30:00.000Z",
                "windows": [window(json!({"usedPercent": 55}))],
            }))}),
        );
        let hub = source(json!({
            "id": "hub",
            "accounts": [{
                "id": "claude-same@example.com.json",
                "driver": "claudeAgent",
                "email": "same@example.com",
                "plan": "Claude Subscription",
                "usageLimits": checked(json!({"windows": [window(json!({"usedPercent": 10}))]})),
            }],
        }));
        let input = map(vec![
            ("env-a", pres("Laptop", vec![native], vec![])),
            ("env-b", pres("Desktop", vec![desktop_native], vec![hub])),
        ]);
        let accounts = collect_limit_accounts(&input);
        assert_eq!(accounts.len(), 1);
        let account = &accounts[0];
        assert_eq!(account.key, "env-a:claude");
        assert_eq!(account.source_label, None);
        // Desktop's read is fresher, so its credits and its redeem are the ones on show.
        assert_eq!(
            account.redeem,
            Some(LimitRedeem {
                environment_id: env("env-b"),
                input: ProviderConsumeResetCreditInput::Instance {
                    instance_id: instance("claude")
                },
            })
        );
        assert_eq!(
            env_label(account),
            vec![("env-a", "Laptop"), ("env-b", "Desktop")]
        );
        // The fresher native snapshot wins; the hub row is pre-filtered by email.
        assert_eq!(account.limits.windows[0].used_percent, 55.0);
    }

    #[test]
    fn merges_opencode_go_limits_from_machines_with_the_same_api_key() {
        let go = provider(json!({
            "driver": "opencode",
            "instanceId": "opencode",
            "auth": {"status": "authenticated"},
            "usageLimits": checked(json!({
                "credentialFingerprint": "shared-go-key",
                "windows": [window(json!({"id": "go_rolling", "usedPercent": 3}))],
            })),
        }));
        let go_b = respread(
            &go,
            json!({"usageLimits": checked(json!({
                "checkedAt": "2026-09-03T11:30:00.000Z",
                "credentialFingerprint": "shared-go-key",
                "windows": [window(json!({"id": "go_rolling", "usedPercent": 4}))],
            }))}),
        );
        let mut input = map(vec![
            ("env-a", laptop(vec![go.clone()])),
            ("env-b", pres("Desktop", vec![go_b], vec![])),
        ]);
        let accounts = collect_limit_accounts(&input);
        assert_eq!(accounts.len(), 1);
        assert_eq!(
            env_label(&accounts[0]),
            vec![("env-a", "Laptop"), ("env-b", "Desktop")]
        );
        assert_eq!(
            collect_limit_pools(&accounts, now())[0].windows[0]
                .members
                .len(),
            1
        );
        assert_eq!(accounts[0].limits.windows[0].used_percent, 4.0);

        let different_key = respread(
            &go,
            json!({"usageLimits": {
                "checkedAt": "2026-09-03T11:00:00.000Z",
                "credentialFingerprint": "other-go-key",
                "windows": [window_json()],
            }}),
        );
        input.insert(
            env("env-b"),
            pres("Desktop", vec![different_key.clone()], vec![]),
        );
        assert_eq!(collect_limit_accounts(&input).len(), 2);

        input.insert(
            env("env-a"),
            laptop(vec![respread(
                &go,
                json!({"auth": {"status": "authenticated", "email": "same@example.com"}}),
            )]),
        );
        input.insert(
            env("env-b"),
            pres(
                "Desktop",
                vec![respread(
                    &different_key,
                    json!({"auth": {"status": "authenticated", "email": "SAME@example.com"}}),
                )],
                vec![],
            ),
        );
        assert_eq!(collect_limit_accounts(&input).len(), 1);
    }

    #[test]
    fn takes_windows_from_a_fresher_hub_read_but_credits_and_redeem_from_the_native_instance() {
        let claude = "claudeAgent";
        let native = provider(json!({
            "driver": claude,
            "instanceId": "claude",
            "auth": {"status": "authenticated", "email": "same@example.com"},
            "usageLimits": checked(json!({"resetCredits": {"availableCount": 2}})),
        }));
        let hub = source(json!({
            "accounts": [{
                "id": "claude-same@example.com.json",
                "driver": claude,
                "email": "same@example.com",
                "usageLimits": checked(json!({
                    "checkedAt": "2026-09-03T11:30:00.000Z",
                    "windows": [window(json!({"usedPercent": 55}))],
                })),
            }],
        }));
        let input = map(vec![("env-a", pres("Laptop", vec![native], vec![hub]))]);
        let accounts = collect_limit_accounts(&input);
        let account = &accounts[0];
        assert_eq!(account.limits.windows[0].used_percent, 55.0);
        assert_eq!(
            account
                .limits
                .reset_credits
                .as_ref()
                .unwrap()
                .available_count,
            2
        );
        assert_eq!(
            account.redeem,
            Some(LimitRedeem {
                environment_id: env("env-a"),
                input: ProviderConsumeResetCreditInput::Instance {
                    instance_id: instance("claude")
                },
            })
        );
        assert_eq!(env_label(account), vec![("env-a", "Laptop")]);
    }

    #[test]
    fn redeems_through_the_hub_when_it_holds_a_credit_even_with_a_fresher_native_read() {
        let claude = "claudeAgent";
        let native = provider(json!({
            "driver": claude,
            "instanceId": "claude",
            "auth": {"status": "authenticated", "email": "same@example.com"},
            "usageLimits": checked(json!({
                "checkedAt": "2026-09-03T11:30:00.000Z",
                "windows": [window(json!({"usedPercent": 40}))],
                "resetCredits": {"availableCount": 3, "nextCreditId": "native-credit"},
            })),
        }));
        let hub = source(json!({
            "accounts": [{
                "id": "claude-same@example.com.json",
                "driver": claude,
                "email": "same@example.com",
                "usageLimits": checked(json!({
                    "windows": [window(json!({"usedPercent": 55}))],
                    "resetCredits": {"availableCount": 2, "nextCreditId": "hub-credit"},
                })),
            }],
        }));
        let input = map(vec![("env-a", pres("Laptop", vec![native], vec![hub]))]);
        let accounts = collect_limit_accounts(&input);
        let account = &accounts[0];
        // Only the hub path clears the routing cooldown it holds for this account.
        assert_eq!(
            account.redeem,
            Some(LimitRedeem {
                environment_id: env("env-a"),
                input: ProviderConsumeResetCreditInput::Source(
                    UsageLimitSourceConsumeResetCreditInput {
                        source_id: UsageLimitSourceId::new("hub"),
                        account_id: TrimmedNonEmptyString::new("claude-same@example.com.json")
                            .unwrap(),
                        credit_id: TrimmedNonEmptyString::new("hub-credit").unwrap(),
                    }
                ),
            })
        );
        // The fresher native balance is still the one shown.
        assert_eq!(
            account
                .limits
                .reset_credits
                .as_ref()
                .unwrap()
                .available_count,
            3
        );
    }

    #[test]
    fn redeems_on_the_environment_whose_snapshot_supplied_the_credits_on_show() {
        let stale = provider(json!({
            "auth": {"status": "authenticated", "email": "same@example.com"},
            "usageLimits": checked(json!({"resetCredits": {"availableCount": 0}})),
        }));
        let fresh = respread(
            &stale,
            json!({"usageLimits": checked(json!({
                "checkedAt": "2026-09-03T11:30:00.000Z",
                "resetCredits": {"availableCount": 2},
            }))}),
        );
        let input = map(vec![
            ("env-a", laptop(vec![stale])),
            ("env-b", pres("Desktop", vec![fresh], vec![])),
        ]);
        let accounts = collect_limit_accounts(&input);
        let account = &accounts[0];
        assert_eq!(
            account
                .limits
                .reset_credits
                .as_ref()
                .unwrap()
                .available_count,
            2
        );
        assert_eq!(
            account.redeem,
            Some(LimitRedeem {
                environment_id: env("env-b"),
                input: ProviderConsumeResetCreditInput::Instance {
                    instance_id: instance("codex")
                },
            })
        );
    }

    #[test]
    fn uses_the_freshest_hub_credit_and_its_environment_even_when_the_account_is_also_native() {
        let native = provider(json!({
            "auth": {"status": "authenticated", "email": "same@example.com"},
            "usageLimits": checked(json!({"resetCredits": {"availableCount": 1}})),
        }));
        let mut hub_account = json!({
            "id": "codex-same.json",
            "driver": "codex",
            "email": "same@example.com",
            "usageLimits": checked(json!({
                "checkedAt": "2026-09-03T11:30:00.000Z",
                "resetCredits": {"availableCount": 2, "nextCreditId": "credit-2"},
            })),
        });
        let hub = |account: Value| source(json!({"accounts": [account]}));
        let input = map(vec![
            ("env-a", laptop(vec![native.clone()])),
            (
                "env-b",
                pres("Desktop", vec![], vec![hub(hub_account.clone())]),
            ),
        ]);
        let accounts = collect_limit_accounts(&input);
        let account = &accounts[0];
        assert_eq!(
            account
                .limits
                .reset_credits
                .as_ref()
                .unwrap()
                .available_count,
            2
        );
        assert_eq!(
            account.redeem,
            Some(LimitRedeem {
                environment_id: env("env-b"),
                input: ProviderConsumeResetCreditInput::Source(
                    UsageLimitSourceConsumeResetCreditInput {
                        source_id: UsageLimitSourceId::new("hub"),
                        account_id: TrimmedNonEmptyString::new("codex-same.json").unwrap(),
                        credit_id: TrimmedNonEmptyString::new("credit-2").unwrap(),
                    }
                ),
            })
        );
        hub_account["usageLimits"]["resetCredits"]["availableCount"] = json!(0);
        let input = map(vec![
            ("env-a", laptop(vec![native])),
            ("env-b", pres("Desktop", vec![], vec![hub(hub_account)])),
        ]);
        assert_eq!(
            collect_limit_accounts(&input)[0]
                .limits
                .reset_credits
                .as_ref()
                .unwrap()
                .available_count,
            0
        );
    }

    #[test]
    fn keeps_distinct_hub_accounts_redeemable_through_their_own_source() {
        let accounts_json: Vec<Value> = ["first", "second"]
            .iter()
            .map(|id| {
                json!({
                    "id": id,
                    "driver": "codex",
                    "email": format!("{id}@example.com"),
                    "usageLimits": checked(json!({
                        "resetCredits": {"availableCount": 2, "nextCreditId": format!("{id}-credit")},
                    })),
                })
            })
            .collect();
        let input = map(vec![(
            "env-a",
            pres(
                "Laptop",
                vec![],
                vec![source(json!({"accounts": accounts_json}))],
            ),
        )]);
        let redeems: Vec<Option<LimitRedeem>> = collect_limit_accounts(&input)
            .into_iter()
            .map(|account| account.redeem)
            .collect();
        let expected: Vec<Option<LimitRedeem>> = ["first", "second"]
            .iter()
            .map(|id| {
                Some(LimitRedeem {
                    environment_id: env("env-a"),
                    input: ProviderConsumeResetCreditInput::Source(
                        UsageLimitSourceConsumeResetCreditInput {
                            source_id: UsageLimitSourceId::new("hub"),
                            account_id: TrimmedNonEmptyString::new(*id).unwrap(),
                            credit_id: TrimmedNonEmptyString::new(format!("{id}-credit")).unwrap(),
                        },
                    ),
                })
            })
            .collect();
        assert_eq!(redeems, expected);
    }

    #[test]
    fn does_not_give_old_credits_the_timestamp_of_a_newer_window_only_read() {
        let snapshots = [
            json!({"checkedAt": "2026-09-03T11:00:00.000Z", "windows": [window_json()], "resetCredits": {"availableCount": 2}}),
            json!({"checkedAt": "2026-09-03T12:00:00.000Z", "windows": [window_json()]}),
            json!({"checkedAt": "2026-09-03T11:30:00.000Z", "windows": [window_json()], "resetCredits": {"availableCount": 1}}),
        ];
        let input = map(snapshots
            .iter()
            .enumerate()
            .map(|(i, usage)| {
                let id: &'static str = Box::leak(format!("env-{i}").into_boxed_str());
                (
                    id,
                    laptop(vec![provider(json!({
                        "auth": {"status": "authenticated", "email": "same@example.com"},
                        "usageLimits": usage,
                    }))]),
                )
            })
            .collect());
        let accounts = collect_limit_accounts(&input);
        let account = &accounts[0];
        assert_eq!(account.limits.checked_at, "2026-09-03T12:00:00.000Z");
        assert_eq!(
            account
                .limits
                .reset_credits
                .as_ref()
                .unwrap()
                .available_count,
            1
        );
        assert_eq!(
            account.redeem.as_ref().unwrap().environment_id,
            env("env-2")
        );
    }

    #[test]
    fn keeps_one_email_in_two_workspaces_apart_while_merging_each_workspaces_duplicates() {
        let login = |instance_id: &str, workspace: &str, used: f64| {
            provider(json!({
                "instanceId": instance_id,
                "auth": {"status": "authenticated", "email": "same@example.com", "workspaceId": workspace},
                "usageLimits": checked(json!({"windows": [weekly_json_with(used)]})),
            }))
        };
        let hub_account = |id: &str, workspace: Option<&str>| {
            let mut value = json!({
                "id": id,
                "driver": "codex",
                "email": "same@example.com",
                "usageLimits": checked(json!({"windows": [weekly_json_with(50.0)]})),
            });
            if let Some(workspace) = workspace {
                value["workspaceId"] = json!(workspace);
            }
            value
        };
        let input = map(vec![
            (
                "env-a",
                pres(
                    "Laptop",
                    vec![
                        login("business", "ws-business", 8.0),
                        login("personal", "ws-plus", 26.0),
                    ],
                    // A hub that names the workspace joins it; one that does not
                    // cannot say which of the two it read, so it keeps its own row.
                    vec![source(json!({"accounts": [
                        hub_account("plus.json", Some("ws-plus")),
                        hub_account("x.json", None),
                    ]}))],
                ),
            ),
            (
                "env-b",
                pres("Desktop", vec![login("personal", "ws-plus", 26.0)], vec![]),
            ),
        ]);
        let accounts = collect_limit_accounts(&input);
        assert_eq!(
            keys(&accounts),
            vec!["env-a:business", "env-a:personal", "hub:x.json"]
        );
        assert_eq!(accounts[0].limits.windows[0].used_percent, 8.0);
        // One hub file name in two workspaces stays two accounts, even without an
        // email to tell them apart.
        let without_email = |id: &str, workspace: &str| {
            let mut value = hub_account(id, Some(workspace));
            value.as_object_mut().unwrap().remove("email");
            value
        };
        let same_file = map(vec![
            (
                "env-a",
                pres(
                    "Laptop",
                    vec![],
                    vec![source(
                        json!({"accounts": [without_email("a.json", "ws-1")]}),
                    )],
                ),
            ),
            (
                "env-b",
                pres(
                    "Desktop",
                    vec![],
                    vec![source(
                        json!({"accounts": [without_email("a.json", "ws-2")]}),
                    )],
                ),
            ),
        ]);
        assert_eq!(
            keys(&collect_limit_accounts(&same_file)),
            vec!["hub:a.json:ws-1", "hub:a.json:ws-2"]
        );
        assert_eq!(
            collect_limit_accounts(&input)[1]
                .environments
                .iter()
                .map(|environment| environment.label.as_str())
                .collect::<Vec<_>>(),
            vec!["Laptop", "Desktop"]
        );
    }

    fn weekly_json_with(used: f64) -> Value {
        merge(weekly_json(), json!({"usedPercent": used}))
    }

    #[test]
    fn joins_a_report_without_a_workspace_to_the_only_workspace_signed_in_with_its_email() {
        let native = provider(json!({
            "driver": "claudeAgent",
            "auth": {"status": "authenticated", "email": "same@example.com", "workspaceId": "org-a"},
            "usageLimits": checked(json!({})),
        }));
        let hub_account = json!({
            "id": "claude.json",
            "driver": "claudeAgent",
            "email": "same@example.com",
            "usageLimits": checked(json!({})),
        });
        let with_other = |other: ServerProvider| {
            map(vec![(
                "env-a",
                pres(
                    "Laptop",
                    vec![native.clone(), other],
                    vec![source(json!({"accounts": [hub_account.clone()]}))],
                ),
            )])
        };
        let unrelated = provider(json!({"instanceId": "codex-2"}));
        assert_eq!(
            keys(&collect_limit_accounts(&with_other(unrelated))),
            vec!["env-a:codex"]
        );
        // A second org whose limits could not be read still makes the hub ambiguous.
        let unreadable = respread(
            &native,
            json!({
                "instanceId": "team",
                "auth": {"status": "authenticated", "email": "same@example.com", "workspaceId": "org-b"},
                "usageLimits": {"checkedAt": "2026-09-03T11:00:00.000Z", "windows": [], "unavailable": {"reason": "probeFailed"}},
            }),
        );
        assert_eq!(
            keys(&collect_limit_accounts(&with_other(unreadable))),
            vec!["env-a:codex", "hub:claude.json"]
        );
    }

    #[test]
    fn names_an_environment_once_however_many_of_its_instances_share_the_account() {
        let shared = provider(json!({
            "auth": {"status": "authenticated", "email": "same@example.com"},
            "usageLimits": checked(json!({})),
        }));
        let other = respread(&shared, json!({"instanceId": "work"}));
        let input = map(vec![("env-a", laptop(vec![shared, other]))]);
        assert_eq!(
            env_label(&collect_limit_accounts(&input)[0]),
            vec![("env-a", "Laptop")]
        );
    }

    #[test]
    fn keys_a_hub_account_without_an_email_by_hub_so_two_environments_on_one_hub_share_it() {
        let seat = json!({
            "id": "claude-team-seat.json",
            "driver": "claudeAgent",
            "usageLimits": checked(json!({})),
        });
        let hub = source(json!({"accounts": [seat]}));
        let input = map(vec![
            ("env-a", pres("Laptop", vec![], vec![hub.clone()])),
            ("env-b", pres("Desktop", vec![], vec![hub])),
        ]);
        let accounts = collect_limit_accounts(&input);
        assert_eq!(keys(&accounts), vec!["hub:claude-team-seat.json"]);
        assert_eq!(
            accounts[0].display_name.as_deref(),
            Some("claude-team-seat")
        );
    }

    fn codex_account(id: &str, slots: &[&str]) -> Value {
        json!({
            "id": id,
            "driver": "codex",
            "usageLimits": {
                "checkedAt": "2026-09-03T11:00:00.000Z",
                "windows": slots.iter().map(|slot| merge(weekly_json(), json!({"id": slot, "usedPercent": 50}))).collect::<Vec<_>>(),
            },
        })
    }

    fn pooled_codex(accounts: Vec<Value>) -> Vec<LimitPoolWindow> {
        let input = map(vec![(
            "env-a",
            pres(
                "Laptop",
                vec![],
                vec![source(json!({"accounts": accounts}))],
            ),
        )]);
        collect_limit_pools(&collect_limit_accounts(&input), now())
            .into_iter()
            .next()
            .map(|pool| pool.windows)
            .unwrap_or_default()
    }

    #[test]
    fn pools_codex_windows_by_kind_whichever_slot_reported_them() {
        // A weekly-only plan reports weekly as `primary`. The pool keeps one id
        // whichever account sorts first.
        for order in [
            vec![
                codex_account("business", &["primary"]),
                codex_account("plus", &["secondary"]),
            ],
            vec![
                codex_account("plus", &["secondary"]),
                codex_account("business", &["primary"]),
            ],
        ] {
            let windows = pooled_codex(order);
            assert_eq!(
                windows.iter().map(|w| w.id.as_str()).collect::<Vec<_>>(),
                vec!["weekly"]
            );
            assert_eq!(windows[0].members.len(), 2);
        }
        // A second window of the same kind on one account stays apart, with its own id.
        let windows = pooled_codex(vec![
            codex_account("a", &["secondary"]),
            codex_account("b", &["primary", "secondary"]),
        ]);
        assert_eq!(
            windows.iter().map(|w| w.id.as_str()).collect::<Vec<_>>(),
            vec!["weekly", "weekly:secondary"]
        );
    }

    #[test]
    fn pools_windows_by_id_across_accounts_and_orders_resets_by_when_they_land() {
        let claude = "claudeAgent";
        let input = map(vec![(
            "env-a",
            pres(
                "Laptop",
                vec![],
                vec![source(json!({"accounts": [
                    {
                        "id": "a",
                        "driver": claude,
                        "usageLimits": {
                            "checkedAt": "2026-09-03T11:00:00.000Z",
                            "windows": [
                                window_value(json!({"usedPercent": 80, "resetsAt": "2026-09-03T13:00:00.000Z"})),
                                weekly_json_with(20.0),
                            ],
                        },
                    },
                    {
                        "id": "b",
                        "driver": claude,
                        "usageLimits": {"checkedAt": "2026-09-03T11:00:00.000Z", "windows": [window_value(json!({"usedPercent": 40}))]},
                    },
                    {
                        "id": "c",
                        "driver": "codex",
                        "usageLimits": {"checkedAt": "2026-09-03T11:00:00.000Z", "windows": [weekly_json_with(50.0)]},
                    },
                    {
                        "id": "unsupported",
                        "driver": claude,
                        "usageLimits": {"checkedAt": "2026-09-03T11:00:00.000Z", "windows": [], "unavailable": {"reason": "unsupported"}},
                    },
                ]}))],
            ),
        )]);
        let pools = collect_limit_pools(&collect_limit_accounts(&input), now());
        assert_eq!(
            pools
                .iter()
                .map(|pool| (pool.driver.as_str(), pool.accounts.len()))
                .collect::<Vec<_>>(),
            vec![("claudeAgent", 2), ("codex", 1)]
        );
        let session = &pools[0].windows[0];
        let week = &pools[0].windows[1];
        // A member with no reset has no clock, so it does not vote on pace.
        let mut untimed_accounts = collect_limit_accounts(&input);
        for account in &mut untimed_accounts {
            if account.key == "hub:b" {
                for w in &mut account.limits.windows {
                    w.resets_at = None;
                }
            }
        }
        let untimed = collect_limit_pools(&untimed_accounts, now());
        // Only a votes: 80% used, 80% elapsed.
        assert_eq!(untimed[0].windows[0].pace, Some(LimitPace::On));
        // a is 80% through its window and b 60%: the pool is 70% elapsed, 60% used.
        assert_eq!(session.id, "five_hour");
        assert_eq!(session.remaining_percent, 40.0);
        assert_eq!(session.used_percent, 60.0);
        assert_eq!(session.pace, Some(LimitPace::Under));
        assert_eq!(
            session
                .resets
                .iter()
                .map(|reset| (reset.member.account.key.as_str(), reset.restores_percent))
                .collect::<Vec<_>>(),
            vec![("hub:a", 40.0), ("hub:b", 20.0)]
        );
        assert_eq!(week.id, "seven_day");
        assert_eq!(week.remaining_percent, 80.0);
        assert_eq!(week.members.len(), 1);
        // Codex reports `primary` for both its five-hour and (on Go) monthly window.
        let mut with_go = collect_limit_accounts(&input);
        with_go.push(LimitAccount {
            key: "go".into(),
            driver: driver(claude),
            display_name: Some("Go".into()),
            email: None,
            plan: None,
            accent_color: None,
            environments: vec![],
            source_label: None,
            redeem: None,
            limits: limits(json!({
                "windows": [{
                    "id": "five_hour",
                    "kind": "monthly",
                    "label": "Monthly",
                    "usedPercent": 82,
                    "windowDurationMins": 30 * 24 * 60,
                    "resetsAt": "2026-09-14T12:00:00.000Z",
                }],
            })),
        });
        let mixed = collect_limit_pools(&with_go, now());
        assert_eq!(
            mixed[0]
                .windows
                .iter()
                .map(|w| (w.kind, w.members.len()))
                .collect::<Vec<_>>(),
            vec![
                (ServerProviderUsageWindowKind::Session, 2),
                (ServerProviderUsageWindowKind::Weekly, 1),
                (ServerProviderUsageWindowKind::Monthly, 1),
            ]
        );
        // Session resets determine the account order for every row.
        assert_eq!(
            session
                .members
                .iter()
                .map(|m| m.account.key.as_str())
                .collect::<Vec<_>>(),
            vec!["hub:a", "hub:b"]
        );
        assert_eq!(
            pools[0]
                .accounts
                .iter()
                .map(|a| a.key.as_str())
                .collect::<Vec<_>>(),
            vec!["hub:a", "hub:b"]
        );
    }

    fn window_value(overrides: Value) -> Value {
        serde_json::to_value(window(overrides)).expect("window encodes")
    }

    fn pooled_account(key: &str, windows: Vec<Value>) -> LimitAccount {
        LimitAccount {
            key: key.to_owned(),
            driver: driver("claudeAgent"),
            display_name: Some(key.to_owned()),
            email: None,
            plan: None,
            accent_color: None,
            environments: vec![],
            source_label: Some("Hub".to_owned()),
            redeem: None,
            limits: limits(json!({
                "checkedAt": "2026-09-03T11:00:00.000Z",
                "windows": windows,
            })),
        }
    }

    fn column_keys(pool: &LimitPool) -> Vec<Vec<Option<&str>>> {
        pool.windows
            .iter()
            .map(|row| {
                row.columns
                    .iter()
                    .map(|member| member.window.as_ref().map(|_| member.account.key.as_str()))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn keeps_session_columns_across_rows_with_opposite_reset_and_usage_orders() {
        let weekly = weekly_json_with(0.0);
        let accounts = vec![
            pooled_account(
                "a",
                vec![
                    merge(
                        weekly.clone(),
                        json!({"usedPercent": 80, "resetsAt": "2026-09-05T12:00:00.000Z"}),
                    ),
                    window_value(
                        json!({"usedPercent": 10, "resetsAt": "2026-09-03T15:00:00.000Z"}),
                    ),
                ],
            ),
            pooled_account(
                "b",
                vec![
                    merge(
                        weekly,
                        json!({"usedPercent": 20, "resetsAt": "2026-09-06T12:00:00.000Z"}),
                    ),
                    window_value(
                        json!({"usedPercent": 90, "resetsAt": "2026-09-03T13:00:00.000Z"}),
                    ),
                ],
            ),
        ];
        let pools = collect_limit_pools(&accounts, now());
        let pool = &pools[0];
        assert_eq!(
            pool.accounts
                .iter()
                .map(|a| a.key.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "a"]
        );
        assert_eq!(
            column_keys(pool),
            vec![vec![Some("b"), Some("a")], vec![Some("b"), Some("a")]]
        );
        assert_eq!(
            pool.windows[1]
                .resets
                .iter()
                .map(|reset| reset.member.account.key.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert_eq!(pool.windows[1].remaining_percent, 50.0);
        let reversed: Vec<LimitAccount> = accounts.iter().rev().cloned().collect();
        assert_eq!(
            column_keys(&collect_limit_pools(&reversed, now())[0]),
            column_keys(pool)
        );
    }

    #[test]
    fn preserves_gaps_without_counting_missing_windows_toward_pooled_quota() {
        let window_only = window_value(json!({}));
        let accounts = vec![
            pooled_account("a", vec![window_only.clone()]),
            pooled_account(
                "b",
                vec![
                    window_value(json!({"resetsAt": "2026-09-03T15:00:00.000Z"})),
                    merge(weekly_json(), json!({"usedPercent": 80})),
                ],
            ),
            pooled_account("c", vec![weekly_json()]),
        ];
        let pools = collect_limit_pools(&accounts, now());
        let pool = &pools[0];
        assert_eq!(
            column_keys(pool),
            vec![
                vec![Some("a"), Some("b"), None],
                vec![None, Some("b"), Some("c")],
            ]
        );
        assert_eq!(
            pool.windows[1]
                .members
                .iter()
                .map(|m| m.account.key.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "c"]
        );
        assert_eq!(pool.windows[1].remaining_percent, 40.0);
        assert_eq!(
            pool.windows[1]
                .resets
                .iter()
                .map(|r| r.restores_percent)
                .collect::<Vec<_>>(),
            vec![40.0, 20.0]
        );
    }

    #[test]
    fn falls_back_to_weekly_resets_when_no_account_reports_a_session() {
        let accounts = vec![
            pooled_account(
                "a",
                vec![merge(
                    weekly_json(),
                    json!({"resetsAt": "2026-09-06T12:00:00.000Z"}),
                )],
            ),
            pooled_account(
                "b",
                vec![merge(
                    weekly_json(),
                    json!({"resetsAt": "2026-09-05T12:00:00.000Z"}),
                )],
            ),
        ];
        let pools = collect_limit_pools(&accounts, now());
        assert_eq!(column_keys(&pools[0]), vec![vec![Some("b"), Some("a")]]);
    }

    #[test]
    fn sorts_unknown_resets_last_and_breaks_ties_consistently() {
        let accounts = vec![
            pooled_account("z", vec![window_value(json!({"resetsAt": null}))]),
            pooled_account("b", vec![window_value(json!({}))]),
            pooled_account("a", vec![window_value(json!({}))]),
            pooled_account("y", vec![window_value(json!({"resetsAt": "invalid"}))]),
        ];
        let pools = collect_limit_pools(&accounts, now());
        assert_eq!(
            column_keys(&pools[0]),
            vec![vec![Some("a"), Some("b"), Some("y"), Some("z")]]
        );
        let reversed: Vec<LimitAccount> = accounts.iter().rev().cloned().collect();
        assert_eq!(
            column_keys(&collect_limit_pools(&reversed, now())[0]),
            vec![vec![Some("a"), Some("b"), Some("y"), Some("z")]]
        );
    }

    fn cursor_account() -> LimitAccount {
        LimitAccount {
            key: "cursor".into(),
            driver: driver("cursor"),
            display_name: Some("Cursor".into()),
            email: None,
            plan: None,
            accent_color: None,
            environments: vec![],
            source_label: Some("Cursor".into()),
            redeem: None,
            limits: limits(json!({
                "checkedAt": "2026-09-03T11:00:00.000Z",
                "windows": [
                    {"id": "apiPercentUsed", "kind": "monthly", "label": "Other Models", "usedPercent": 49},
                    {"id": "autoPercentUsed", "kind": "monthly", "label": "Cursor Models", "usedPercent": 9},
                    {"id": "totalPercentUsed", "kind": "monthly", "label": "Overall", "usedPercent": 15},
                ],
            })),
        }
    }

    #[test]
    fn cursor_hides_the_combined_percentage_and_orders_the_two_pools() {
        let pools = collect_limit_pools(&[cursor_account()], now());
        let display = display_limit_windows(&pools[0]);
        assert_eq!(
            display.iter().map(|w| w.id.as_str()).collect::<Vec<_>>(),
            vec!["autoPercentUsed", "apiPercentUsed"]
        );
    }

    #[test]
    fn cursor_keeps_the_combined_percentage_as_a_card_if_either_allowance_is_missing() {
        let mut account = cursor_account();
        account
            .limits
            .windows
            .retain(|w| w.id.as_str() != "apiPercentUsed");
        let pools = collect_limit_pools(&[account], now());
        let display = display_limit_windows(&pools[0]);
        assert_eq!(
            display.iter().map(|w| w.id.as_str()).collect::<Vec<_>>(),
            vec!["totalPercentUsed", "autoPercentUsed"]
        );
    }

    #[test]
    fn collect_limit_notices_names_failures_and_silence_skips_unsupported_and_labels_when_several()
    {
        let claude = "claudeAgent";
        let failed = provider(json!({
            "instanceId": "claude",
            "driver": claude,
            "displayName": "Claude Max",
            "usageLimits": {"checkedAt": "2026-09-03T11:00:00.000Z", "windows": [], "unavailable": {"reason": "probeFailed"}},
        }));
        let api_key = provider(json!({
            "instanceId": "api",
            "driver": claude,
            "usageLimits": {"checkedAt": "2026-09-03T11:00:00.000Z", "windows": [], "unavailable": {"reason": "unsupported"}},
        }));
        let silent = provider(
            json!({"usageLimits": {"checkedAt": "2026-09-03T11:00:00.000Z", "windows": []}}),
        );
        let hub = |id: &str, label: &str, extra: Value| {
            source(merge(
                json!({"id": id, "label": label, "accounts": []}),
                extra,
            ))
        };
        let mut input = map(vec![(
            "env-a",
            pres(
                "Laptop",
                vec![failed, api_key, silent],
                vec![
                    hub("hub", "hub", json!({})),
                    hub("down", "down", json!({"error": "ECONNREFUSED"})),
                ],
            ),
        )]);
        assert_eq!(
            collect_limit_notices(&input),
            vec![
                "Claude Max: Could not read limits.",
                "codex: No limits reported.",
                "hub: No accounts reported.",
                "down: ECONNREFUSED",
            ]
        );
        input.insert(env("env-b"), pres("Desktop", vec![], vec![]));
        assert_eq!(
            collect_limit_notices(&input)[0],
            "Laptop · Claude Max: Could not read limits."
        );
    }

    fn accounts_source(accounts: Vec<Value>) -> UsageLimitSourceSnapshot {
        source(json!({"id": "hub", "label": "Accounts", "accounts": accounts}))
    }

    fn command_names(providers: &[ServerProvider]) -> Vec<String> {
        providers
            .iter()
            .flat_map(|p| p.slash_commands.iter().map(|c| c.name.as_str().to_owned()))
            .collect()
    }

    #[test]
    fn usage_limits_report_uses_hub_credit_balances_and_redemption_targets_including_native_duplicates()
     {
        let limits_json = checked(json!({}));
        let selected = provider(json!({
            "usageLimits": limits_json,
            "auth": {"status": "authenticated", "email": "same@example.com"},
        }));
        let hubs = vec![accounts_source(vec![
            json!({
                "id": "duplicate",
                "driver": "codex",
                "email": "SAME@example.com",
                "usageLimits": merge(limits_json.clone(), json!({"resetCredits": {"availableCount": 2, "nextCreditId": "duplicate-credit"}})),
            }),
            json!({
                "id": "oss",
                "driver": "codex",
                "plan": "Codex OSS",
                "usageLimits": merge(limits_json.clone(), json!({"resetCredits": {"availableCount": 2, "nextCreditId": "oss-credit"}})),
            }),
        ])];
        let report = collect_provider_usage_limits(&instance("codex"), &[selected], &hubs, now())
            .expect("report");
        assert_eq!(
            report.accounts[0]
                .limits
                .reset_credits
                .as_ref()
                .unwrap()
                .available_count,
            2
        );
        assert_eq!(
            report.accounts[0].reset_credit_input,
            Some(ProviderConsumeResetCreditInput::Source(
                UsageLimitSourceConsumeResetCreditInput {
                    source_id: UsageLimitSourceId::new("hub"),
                    account_id: TrimmedNonEmptyString::new("duplicate").unwrap(),
                    credit_id: TrimmedNonEmptyString::new("duplicate-credit").unwrap(),
                }
            ))
        );
        let oss = report
            .accounts
            .iter()
            .find(|a| a.id.as_str() == "hub:oss")
            .unwrap();
        assert_eq!(
            oss.reset_credit_input,
            Some(ProviderConsumeResetCreditInput::Source(
                UsageLimitSourceConsumeResetCreditInput {
                    source_id: UsageLimitSourceId::new("hub"),
                    account_id: TrimmedNonEmptyString::new("oss").unwrap(),
                    credit_id: TrimmedNonEmptyString::new("oss-credit").unwrap(),
                }
            ))
        );
    }

    #[test]
    fn report_keeps_accounts_and_custom_instances_separate_filtering_by_driver() {
        let limits_json = checked(json!({}));
        let selected = provider(json!({
            "usageLimits": limits_json,
            "auth": {"status": "authenticated", "email": "same@example.com"},
        }));
        let sources = vec![accounts_source(vec![
            json!({"id": "duplicate", "driver": "codex", "email": "SAME@example.com", "usageLimits": limits_json.clone()}),
            json!({"id": "oss", "driver": "codex", "plan": "Codex OSS", "usageLimits": limits_json.clone()}),
            json!({"id": "other-provider", "driver": "claude", "usageLimits": limits_json.clone()}),
        ])];
        let providers = vec![
            selected.clone(),
            provider(json!({
                "instanceId": "codex-work",
                "displayName": "Work",
                "usageLimits": merge(limits_json.clone(), json!({"resetCredits": {"availableCount": 2}})),
            })),
            provider(
                json!({"driver": "claudeAgent", "instanceId": "claude", "usageLimits": limits_json.clone()}),
            ),
        ];
        let report = collect_provider_usage_limits(&instance("codex"), &providers, &sources, now())
            .expect("report");
        assert_eq!(report.created_at, "2026-09-03T12:00:00.000Z");
        assert_eq!(
            report
                .accounts
                .iter()
                .map(|a| a.id.as_str())
                .collect::<Vec<_>>(),
            vec!["codex", "codex-work", "hub:oss"]
        );
        assert_eq!(report.accounts[0].instance_id, Some(instance("codex")));
        assert_eq!(
            report.accounts[0]
                .email
                .as_ref()
                .map(|email| email.as_str()),
            selected.auth.email.as_deref()
        );
        assert_eq!(report.accounts[1].display_name.as_deref(), Some("Work"));
        assert_eq!(
            report.accounts[1]
                .limits
                .reset_credits
                .as_ref()
                .unwrap()
                .available_count,
            2
        );
        assert_eq!(report.accounts[2].label.as_str(), "Accounts · oss");
        assert_eq!(
            report.accounts[2].source_label.as_ref().map(|s| s.as_str()),
            Some("CLI Proxy")
        );
        assert_eq!(
            report.accounts[2].plan.as_ref().map(|s| s.as_str()),
            Some("Codex OSS")
        );
        assert!(report.notices.is_empty());
    }

    #[test]
    fn report_supports_a_source_only_provider_and_keeps_duplicates_when_the_native_probe_failed() {
        let limits_json = checked(json!({}));
        let sources = vec![accounts_source(vec![
            json!({"id": "duplicate", "driver": "codex", "email": "SAME@example.com", "usageLimits": limits_json.clone()}),
            json!({"id": "oss", "driver": "codex", "plan": "Codex OSS", "usageLimits": limits_json.clone()}),
            json!({"id": "other-provider", "driver": "claudeAgent", "usageLimits": limits_json.clone()}),
        ])];
        let ids = |providers: &[ServerProvider],
                   sources: &[UsageLimitSourceSnapshot]|
         -> Option<Vec<String>> {
            collect_provider_usage_limits(&instance("codex"), providers, sources, now()).map(
                |report| {
                    report
                        .accounts
                        .iter()
                        .map(|a| a.id.as_str().to_owned())
                        .collect()
                },
            )
        };
        assert_eq!(
            ids(&[provider(json!({}))], &sources),
            Some(vec!["hub:duplicate".to_owned(), "hub:oss".to_owned()])
        );
        let failed = provider(json!({
            "usageLimits": merge(limits_json.clone(), json!({"unavailable": {"reason": "probeFailed"}})),
        }));
        assert_eq!(
            ids(&[failed], &sources),
            Some(vec![
                "codex".to_owned(),
                "hub:duplicate".to_owned(),
                "hub:oss".to_owned()
            ])
        );
        assert_eq!(ids(&[provider(json!({}))], &[]), None);
        assert_eq!(
            ids(
                &[provider(
                    json!({"enabled": false, "usageLimits": limits_json.clone()})
                )],
                &[]
            ),
            None
        );
    }

    #[test]
    fn report_surfaces_source_errors_only_for_sources_that_carry_the_selected_driver() {
        let limits_json = checked(json!({}));
        let selected = provider(json!({"usageLimits": limits_json.clone()}));
        let accounts = vec![
            json!({"id": "duplicate", "driver": "codex", "email": "SAME@example.com", "usageLimits": limits_json.clone()}),
            json!({"id": "oss", "driver": "codex", "usageLimits": limits_json.clone()}),
            json!({"id": "other-provider", "driver": "claudeAgent", "usageLimits": limits_json.clone()}),
        ];
        let failing = source(
            json!({"id": "hub", "label": "Accounts", "accounts": accounts.clone(), "error": "token expired"}),
        );
        let notices = |providers: &[ServerProvider], sources: &[UsageLimitSourceSnapshot]| {
            collect_provider_usage_limits(&instance("codex"), providers, sources, now())
                .expect("report")
                .notices
        };
        assert_eq!(
            notices(
                std::slice::from_ref(&selected),
                std::slice::from_ref(&failing)
            ),
            vec!["Accounts: token expired"]
        );
        let claude_only = source(
            json!({"id": "hub", "label": "Accounts", "accounts": [accounts[2].clone()], "error": "token expired"}),
        );
        assert!(notices(std::slice::from_ref(&selected), &[claude_only]).is_empty());
        // A read failure clears the accounts, so the error must not depend on a match.
        let unreadable = source(
            json!({"id": "hub", "label": "Accounts", "accounts": [], "error": "token expired"}),
        );
        assert_eq!(
            notices(
                std::slice::from_ref(&selected),
                std::slice::from_ref(&unreadable)
            ),
            vec!["Accounts: token expired"]
        );
        // A source-only provider still gets the report, carrying only the error.
        let source_only = collect_provider_usage_limits(
            &instance("codex"),
            &[provider(json!({}))],
            &[unreadable],
            now(),
        )
        .expect("report");
        assert!(source_only.accounts.is_empty());
        assert_eq!(source_only.notices, vec!["Accounts: token expired"]);
    }

    #[test]
    fn report_gives_a_hub_credit_to_no_native_row_when_its_email_is_signed_in_to_two_workspaces() {
        let limits_json = checked(json!({}));
        let workspaces: Vec<ServerProvider> = ["ws-a", "ws-b"]
            .iter()
            .map(|workspace| {
                provider(json!({
                    "instanceId": workspace,
                    "usageLimits": limits_json.clone(),
                    "auth": {"status": "authenticated", "email": "same@example.com", "workspaceId": workspace},
                }))
            })
            .collect();
        let hub = vec![accounts_source(vec![json!({
            "id": "duplicate",
            "driver": "codex",
            "email": "same@example.com",
            "usageLimits": merge(limits_json.clone(), json!({"resetCredits": {"availableCount": 2, "nextCreditId": "c"}})),
        })])];
        let report =
            collect_provider_usage_limits(&workspaces[0].instance_id, &workspaces, &hub, now())
                .expect("report");
        assert_eq!(
            report
                .accounts
                .iter()
                .map(|a| a.reset_credit_input.clone())
                .collect::<Vec<_>>(),
            vec![
                Some(ProviderConsumeResetCreditInput::Instance {
                    instance_id: instance("ws-a")
                }),
                Some(ProviderConsumeResetCreditInput::Instance {
                    instance_id: instance("ws-b")
                }),
                Some(ProviderConsumeResetCreditInput::Source(
                    UsageLimitSourceConsumeResetCreditInput {
                        source_id: UsageLimitSourceId::new("hub"),
                        account_id: TrimmedNonEmptyString::new("duplicate").unwrap(),
                        credit_id: TrimmedNonEmptyString::new("c").unwrap(),
                    }
                )),
            ]
        );
    }

    #[test]
    fn report_redeems_a_native_duplicate_through_the_hub_even_when_the_native_snapshot_is_fresher()
    {
        let fresher = provider(json!({
            "usageLimits": checked(json!({
                "checkedAt": "2026-09-03T11:30:00.000Z",
                "resetCredits": {"availableCount": 3, "nextCreditId": "native-credit"},
            })),
            "auth": {"status": "authenticated", "email": "same@example.com"},
        }));
        let stale = vec![accounts_source(vec![json!({
            "id": "duplicate",
            "driver": "codex",
            "email": "SAME@example.com",
            "usageLimits": merge(checked(json!({})), json!({"resetCredits": {"availableCount": 2, "nextCreditId": "hub-credit"}})),
        })])];
        let report = collect_provider_usage_limits(
            &fresher.instance_id,
            std::slice::from_ref(&fresher),
            &stale,
            now(),
        )
        .expect("report");
        // Only redeeming through the hub clears the routing cooldown it holds for
        // this account, so the hub wins the path even with a staler balance.
        assert_eq!(
            report.accounts[0].reset_credit_input,
            Some(ProviderConsumeResetCreditInput::Source(
                UsageLimitSourceConsumeResetCreditInput {
                    source_id: UsageLimitSourceId::new("hub"),
                    account_id: TrimmedNonEmptyString::new("duplicate").unwrap(),
                    credit_id: TrimmedNonEmptyString::new("hub-credit").unwrap(),
                }
            ))
        );
        // The fresher native balance is still the one shown.
        assert_eq!(
            report.accounts[0]
                .limits
                .reset_credits
                .as_ref()
                .unwrap()
                .available_count,
            3
        );
    }

    #[test]
    fn advertises_global_and_workspace_commands_only_for_providers_present_in_limits() {
        let limits_json = checked(json!({}));
        let selected = provider(json!({
            "usageLimits": limits_json.clone(),
            "auth": {"status": "authenticated", "email": "same@example.com"},
        }));
        let sources = vec![accounts_source(vec![
            json!({"id": "oss", "driver": "codex", "usageLimits": limits_json.clone()}),
        ])];
        let with_workspace = provider(json!({
            "workspaceSnapshots": [{"cwd": "/tmp/project", "checkedAt": limits_json["checkedAt"], "slashCommands": [], "skills": []}],
        }));
        let supported = with_usage_limits_commands(std::slice::from_ref(&with_workspace), &sources)
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(
            command_names(std::slice::from_ref(&supported)),
            vec!["usage-limits"]
        );
        let snapshot_commands: Vec<&str> = supported.workspace_snapshots.as_ref().unwrap()[0]
            .slash_commands
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(snapshot_commands, vec!["usage-limits"]);
        assert!(
            with_usage_limits_commands(std::slice::from_ref(&with_workspace), &[])[0]
                .slash_commands
                .is_empty()
        );
        // A provider's own command of the same name is left alone without coverage.
        let own_command = provider(json!({
            "slashCommands": [{"name": "usage-limits", "description": "Provider's own"}],
        }));
        let kept = with_usage_limits_commands(std::slice::from_ref(&own_command), &[]);
        assert_eq!(kept[0].slash_commands, own_command.slash_commands);
        let unreadable = source(
            json!({"id": "hub", "label": "Accounts", "accounts": [], "error": "token expired"}),
        );
        assert_eq!(
            command_names(&with_usage_limits_commands(
                std::slice::from_ref(&with_workspace),
                &[unreadable]
            )),
            vec!["usage-limits"]
        );
        assert_eq!(
            command_names(&with_usage_limits_commands(
                std::slice::from_ref(&selected),
                &[]
            )),
            vec!["usage-limits"]
        );
    }

    fn coverage_account(overrides: Value) -> Value {
        merge(
            json!({
                "id": "a",
                "driver": "codex",
                "usageLimits": {"checkedAt": "2026-09-03T11:00:00.000Z", "windows": []},
            }),
            overrides,
        )
    }

    #[test]
    fn same_usage_limit_command_coverage_ignores_quota_movement_but_not_the_drivers_offered_the_command()
     {
        let with_codex = vec![source(json!({"accounts": [coverage_account(json!({}))]}))];
        let with_codex_later = vec![source(json!({"accounts": [coverage_account(json!({
            "usageLimits": {"checkedAt": "2026-09-03T12:00:00.000Z", "windows": []},
        }))]}))];
        assert!(same_usage_limit_command_coverage(
            &with_codex,
            &with_codex_later
        ));
        assert!(!same_usage_limit_command_coverage(
            &with_codex,
            &[source(json!({"accounts": []}))]
        ));
    }

    #[test]
    fn same_usage_limit_command_coverage_treats_a_failed_read_as_a_change_in_coverage_both_ways() {
        let empty = vec![source(json!({"accounts": []}))];
        let failed = vec![source(json!({"accounts": [], "error": "token expired"}))];
        assert!(!same_usage_limit_command_coverage(&empty, &failed));
        assert!(!same_usage_limit_command_coverage(&failed, &empty));
        assert!(same_usage_limit_command_coverage(
            &failed,
            &[source(json!({"accounts": [], "error": "still down"}))]
        ));
    }

    #[test]
    fn remaining_percent_inverts_and_clamps_the_reported_usage() {
        assert_eq!(remaining_percent(&window(json!({}))), 60.0);
        assert_eq!(remaining_percent(&window(json!({"usedPercent": 0}))), 100.0);
        assert_eq!(remaining_percent(&window(json!({"usedPercent": 100}))), 0.0);
        assert_eq!(
            remaining_percent(&window(json!({"usedPercent": 33.4}))),
            67.0
        );
    }

    #[test]
    fn is_usage_limits_command_recognizes_only_the_standalone_local_action() {
        assert!(is_usage_limits_command("  /USAGE-LIMITS\n"));
        assert!(!is_usage_limits_command("/usage-limits explain"));
        assert!(!is_usage_limits_command("Explain /usage-limits"));
        assert!(!is_usage_limits_command("/usage"));
    }

    fn managed_external(overrides: Value) -> ServerProvider {
        provider(merge(
            json!({
                "usageLimits": {
                    "checkedAt": "2026-09-03T11:00:00.000Z",
                    "windows": [],
                    "externalUsage": {"label": "ChatGPT usage", "url": CHATGPT_USAGE_URL},
                },
            }),
            overrides,
        ))
    }

    #[test]
    fn external_links_deduplicate_destinations_across_accounts_and_environments_without_inventing_pools()
     {
        let managed = provider(json!({
            "usageLimits": {
                "checkedAt": "2026-09-03T11:00:00.000Z",
                "windows": [],
                "unavailable": {"reason": "unsupported", "message": "Track usage in ChatGPT."},
                "externalUsage": {"label": "ChatGPT usage", "url": CHATGPT_USAGE_URL},
            },
        }));
        let personal = respread(&managed, json!({"instanceId": "personal"}));
        let presentations = map(vec![
            ("a", pres("A", vec![managed.clone(), personal], vec![])),
            ("b", pres("B", vec![managed.clone()], vec![])),
        ]);
        assert_eq!(
            collect_external_usage_links(&presentations),
            vec![ExternalUsageLink {
                label: "ChatGPT usage".to_owned(),
                url: CHATGPT_USAGE_URL.to_owned(),
                message: Some("Track usage in ChatGPT.".to_owned()),
                accounts: vec![
                    "codex on A".to_owned(),
                    "personal on A".to_owned(),
                    "codex on B".to_owned(),
                ],
            }]
        );
        assert!(collect_limit_accounts(&presentations).is_empty());
        assert!(collect_limit_notices(&presentations).is_empty());
    }

    #[test]
    fn external_links_omit_disabled_uninstalled_and_signed_out_providers() {
        let managed = managed_external(json!({}));
        let presentations = map(vec![(
            "a",
            pres(
                "A",
                vec![
                    respread(&managed, json!({"enabled": false})),
                    respread(&managed, json!({"installed": false})),
                    respread(&managed, json!({"auth": {"status": "unauthenticated"}})),
                    provider(json!({})),
                ],
                vec![],
            ),
        )]);
        assert!(collect_external_usage_links(&presentations).is_empty());
    }

    #[test]
    fn chatgpt_sharing_requires_verified_sharing_metadata_rather_than_the_codex_driver_or_login_type()
     {
        let codex = provider(json!({"auth": {"status": "authenticated", "type": "chatgpt"}}));
        assert!(!uses_chatgpt_sharing(Some(&codex)));
        assert!(uses_chatgpt_sharing(Some(&respread(
            &codex,
            json!({"auth": {"status": "authenticated", "type": "chatgpt", "subscriptionSharing": true}}),
        ))));
        assert!(!uses_chatgpt_sharing(Some(&respread(
            &codex,
            json!({"auth": {"status": "unauthenticated", "subscriptionSharing": true}}),
        ))));
        assert!(!uses_chatgpt_sharing::<FakeProvider>(None));
    }

    #[test]
    fn antigravity_keeps_the_shared_claude_gpt_bucket_separate_from_gemini_and_pairs_their_windows()
    {
        let antigravity = provider(json!({
            "instanceId": "antigravity",
            "driver": "antigravity",
            "usageLimits": {
                "checkedAt": "2026-10-09T12:00:00.000Z",
                "windows": [
                    window_value(json!({"id": "3p-5h", "label": "Claude & GPT (shared) · 5-hour", "usedPercent": 90})),
                    window_value(json!({"id": "gemini-5h", "label": "Gemini · 5-hour", "usedPercent": 20})),
                    window_value(json!({"id": "3p-weekly", "kind": "weekly", "label": "Claude & GPT (shared) · Weekly", "usedPercent": 100})),
                    window_value(json!({"id": "gemini-weekly", "kind": "weekly", "label": "Gemini · Weekly", "usedPercent": 10})),
                ],
            },
        }));
        let codex = provider(json!({
            "usageLimits": {"checkedAt": "2026-10-09T12:00:00.000Z", "windows": [window_value(json!({}))]},
        }));
        let accounts = collect_limit_accounts(&map(vec![(
            "a",
            pres("Local", vec![codex, antigravity], vec![]),
        )]));
        let pools = collect_limit_pools(&accounts, now());
        assert_eq!(
            pools.iter().map(|p| p.driver.as_str()).collect::<Vec<_>>(),
            vec!["codex", "antigravity"]
        );
        let windows = display_limit_windows(&pools[1]);
        assert_eq!(
            windows.iter().map(|w| w.id.as_str()).collect::<Vec<_>>(),
            vec!["gemini-5h", "gemini-weekly", "3p-5h", "3p-weekly"]
        );
        assert_eq!(
            windows
                .iter()
                .map(|w| w.remaining_percent)
                .collect::<Vec<_>>(),
            vec![80.0, 90.0, 10.0, 0.0]
        );
    }
}
