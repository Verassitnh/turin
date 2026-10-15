//! Port of upstream/packages/contracts/src/providerUsageLimits.ts

use crate::base_schemas::{
    IsoDateTime, NonNegativeInt, TrimmedNonEmptyString, forward_compatible_array,
};
use crate::provider_instance::{ProviderDriverKind, ProviderInstanceId};
use crate::usage_limit_source_id::UsageLimitSourceId;
use serde::{Deserialize, Deserializer, Serialize};

/// Serde bound shared by `usedPercent` (0..=100, upstream `Schema.Number`).
mod used_percent {
    use super::*;

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
        let value = f64::deserialize(d)?;
        if (0.0..=100.0).contains(&value) {
            Ok(value)
        } else {
            Err(serde::de::Error::custom(
                "usedPercent must be between 0 and 100",
            ))
        }
    }
}

/// One rolling quota window a subscription provider reports for the
/// signed-in account, e.g. Claude's five-hour session or Codex's weekly
/// allowance.
///
/// `id` is stable per provider (`five_hour`, `seven_day_opus`, `primary`) so
/// a sparse turn-driven update lands on the same row a full probe produced.
/// `kind` only orders and labels the bar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderUsageWindow {
    pub id: TrimmedNonEmptyString,
    pub kind: ServerProviderUsageWindowKind,
    pub label: TrimmedNonEmptyString,
    #[serde(deserialize_with = "used_percent::deserialize")]
    pub used_percent: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<IsoDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_duration_mins: Option<NonNegativeInt>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ServerProviderUsageWindowKind {
    Session,
    Weekly,
    Monthly,
    Other,
}

/// Reset credits a provider banks on the account. Codex grants these when it
/// has rate-limited the user unfairly; redeeming one clears the current
/// windows. Only present when the provider reports them at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderResetCredits {
    pub available_count: NonNegativeInt,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_expires_at: Option<IsoDateTime>,
    /// Pins hub redemption to the displayed credit, including retries from another client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_credit_id: Option<TrimmedNonEmptyString>,
}

/// Why the provider cannot report windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ServerProviderUsageUnavailableReason {
    Unsupported,
    ProbeFailed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderUsageUnavailable {
    pub reason: ServerProviderUsageUnavailableReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<TrimmedNonEmptyString>,
}

/// Provider-owned usage settings when quota windows are not available to the
/// client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderExternalUsage {
    pub label: TrimmedNonEmptyString,
    pub url: TrimmedNonEmptyString,
}

/// Subscription usage the provider knows about the signed-in account.
///
/// `unavailable` distinguishes an account that can never report windows (API
/// key, Bedrock) from a probe that failed this time, so clients can keep the
/// last good bars for the latter and clear them for the former.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderUsageLimits {
    pub checked_at: IsoDateTime,
    #[serde(with = "forward_compatible_array")]
    pub windows: Vec<ServerProviderUsageWindow>,
    /// Opaque credential identity when the provider does not report an account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_fingerprint: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_credits: Option<ServerProviderResetCredits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_usage: Option<ServerProviderExternalUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<ServerProviderUsageUnavailable>,
}

/// What an adapter reports when its runtime pushes a rate-limit update during
/// a turn. Sparse by contract: Claude's `rate_limit_event` names one window at
/// a time and Codex documents its notification as a partial. Windows merge by
/// `id` onto the instance's published snapshot; omitted windows are unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUsageLimitsUpdate {
    pub windows: Vec<ServerProviderUsageWindow>,
}

/// One account a usage-limit source reports on. `driver` is the provider the
/// account belongs to, for the icon and colour clients already have; the
/// account itself is not something this environment can run turns on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimitSourceAccount {
    pub id: TrimmedNonEmptyString,
    pub driver: ProviderDriverKind,
    /// The signed-in address, when the source names one; clients blur it like provider auth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<TrimmedNonEmptyString>,
    /// Plan as the matching provider would label it (`ChatGPT Pro 20x Subscription`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<TrimmedNonEmptyString>,
    /// The workspace the quota belongs to, when the source names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<TrimmedNonEmptyString>,
    pub usage_limits: ServerProviderUsageLimits,
}

string_literal!(
    /// Constant `"cliproxy"` kind of a usage-limit source snapshot.
    CliproxySourceKind,
    "cliproxy"
);

/// The published state of one configured `usageLimitSources` entry. A source
/// that could not be read keeps `error` beside an empty account list rather
/// than vanishing, so the user can see it is configured but failing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimitSourceSnapshot {
    pub id: UsageLimitSourceId,
    pub kind: CliproxySourceKind,
    pub label: TrimmedNonEmptyString,
    pub checked_at: IsoDateTime,
    #[serde(with = "forward_compatible_array")]
    pub accounts: Vec<UsageLimitSourceAccount>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<TrimmedNonEmptyString>,
}

/// The list of snapshots, itself forward-compatible (unknown element shapes
/// drop). Apply `#[serde(with = "forward_compatible_array")]` on fields.
pub type UsageLimitSourceSnapshots = Vec<UsageLimitSourceSnapshot>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimitSourceConsumeResetCreditInput {
    pub source_id: UsageLimitSourceId,
    pub account_id: TrimmedNonEmptyString,
    pub credit_id: TrimmedNonEmptyString,
}

/// `ProviderConsumeResetCreditInput`: an instance-owned credit or a
/// source-owned one, told apart by their keys (untagged union upstream).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged, rename_all_fields = "camelCase")]
pub enum ProviderConsumeResetCreditInput {
    Instance { instance_id: ProviderInstanceId },
    Source(UsageLimitSourceConsumeResetCreditInput),
}

/// `UsageLimitSourceError` (Schema.TaggedError).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum UsageLimitSourceError {
    #[serde(rename = "UsageLimitSourceError")]
    #[error("{detail}")]
    SourceError { detail: String },
}

/// Mirrors Codex's own outcome set; other providers map onto it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderConsumeResetCreditOutcome {
    Reset,
    NothingToReset,
    NoCredit,
    AlreadyRedeemed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConsumeResetCreditResult {
    pub outcome: ProviderConsumeResetCreditOutcome,
    /// Redemption succeeded, but a follow-up such as clearing the hub cooldown failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<TrimmedNonEmptyString>,
}

/// One account row of a [`UsageLimitsReport`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimitsReportAccount {
    pub id: TrimmedNonEmptyString,
    pub driver: ProviderDriverKind,
    pub label: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_label: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<ProviderInstanceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_credit_input: Option<ProviderConsumeResetCreditInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent_color: Option<String>,
    pub limits: ServerProviderUsageLimits,
}

/// A point-in-time view of one provider's limits, built for the
/// /usage-limits panel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimitsReport {
    pub created_at: IsoDateTime,
    pub accounts: Vec<UsageLimitsReportAccount>,
    pub notices: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn window(id: &str, used: f64) -> serde_json::Value {
        json!({"id": id, "kind": "session", "label": "Session", "usedPercent": used})
    }

    #[test]
    fn window_bounds_and_optional_fields() {
        let ok: ServerProviderUsageWindow = serde_json::from_value(json!({
            "id": " five_hour ", "kind": "weekly", "label": "L", "usedPercent": 42.5,
            "resetsAt": "2026-01-01T00:00:00Z", "windowDurationMins": 300,
        }))
        .unwrap();
        assert_eq!(ok.id.as_str(), "five_hour");
        assert_eq!(ok.used_percent, 42.5);
        // IsoDateTime is a pass-through string.
        assert_eq!(ok.resets_at.as_deref(), Some("2026-01-01T00:00:00Z"));
        assert_eq!(
            serde_json::to_value(&window_ok()).unwrap()["usedPercent"],
            json!(0.0)
        );
        for used in [-0.1, 100.1] {
            assert!(
                serde_json::from_value::<ServerProviderUsageWindow>(window("a", used)).is_err()
            );
        }
        assert!(serde_json::from_value::<ServerProviderUsageWindow>(window("a", 0.0)).is_ok());
        assert!(serde_json::from_value::<ServerProviderUsageWindow>(window("a", 100.0)).is_ok());
        assert!(
            serde_json::from_value::<ServerProviderUsageWindow>(
                json!({"id": "a", "kind": "yearly", "label": "L", "usedPercent": 1})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<ServerProviderUsageWindow>(
                json!({"id": "a", "kind": "other", "label": "L", "usedPercent": 1, "windowDurationMins": -1})
            )
            .is_err()
        );
    }

    fn window_ok() -> ServerProviderUsageWindow {
        serde_json::from_value(window("a", 0.0)).unwrap()
    }

    #[test]
    fn limits_windows_drop_undecodable_entries() {
        let limits: ServerProviderUsageLimits = serde_json::from_value(json!({
            "checkedAt": "2026-01-01T00:00:00Z",
            "windows": [window("a", 10.0), window("b", 500.0), {"id": "c", "kind": "newKind"}, window("d", 1.0)],
            "unavailable": {"reason": "probeFailed", "message": "boom"},
            "externalUsage": {"label": "Dashboard", "url": "https://x"},
            "resetCredits": {"availableCount": 2, "nextCreditId": "c1"},
        }))
        .unwrap();
        let ids: Vec<_> = limits.windows.iter().map(|w| w.id.as_str()).collect();
        assert_eq!(ids, ["a", "d"]);
        let encoded = serde_json::to_value(&limits).unwrap();
        assert_eq!(encoded["windows"].as_array().unwrap().len(), 2);
        assert_eq!(encoded["unavailable"]["reason"], json!("probeFailed"));
        assert!(encoded.get("credentialFingerprint").is_none());
        // `windows` is required even when empty.
        assert!(
            serde_json::from_value::<ServerProviderUsageLimits>(json!({"checkedAt": "x"})).is_err()
        );
        assert!(
            serde_json::from_value::<ServerProviderUsageLimits>(
                json!({"checkedAt": "x", "windows": [], "unavailable": {"reason": "nope"}})
            )
            .is_err()
        );
    }

    #[test]
    fn source_snapshot_keeps_literal_kind_and_drops_bad_accounts() {
        let account = |driver: &str| {
            json!({
                "id": "acct", "driver": driver,
                "usageLimits": {"checkedAt": "x", "windows": []},
            })
        };
        let snapshot: UsageLimitSourceSnapshot = serde_json::from_value(json!({
            "id": "src_1", "kind": "cliproxy", "label": "Proxy", "checkedAt": "x",
            "accounts": [account("codex"), account("1bad")],
        }))
        .unwrap();
        assert_eq!(snapshot.accounts.len(), 1);
        assert!(
            serde_json::from_value::<UsageLimitSourceSnapshot>(
                json!({"id": "src_1", "kind": "other", "label": "P", "checkedAt": "x", "accounts": []})
            )
            .is_err()
        );
    }

    #[test]
    fn consume_reset_credit_input_union_uses_camel_case_keys() {
        let instance: ProviderConsumeResetCreditInput =
            serde_json::from_value(json!({"instanceId": "codex"})).unwrap();
        assert!(matches!(
            instance,
            ProviderConsumeResetCreditInput::Instance { .. }
        ));
        assert_eq!(
            serde_json::to_value(&instance).unwrap(),
            json!({"instanceId": "codex"})
        );
        let source_json = json!({"sourceId": "src_1", "accountId": "a", "creditId": "c"});
        let source: ProviderConsumeResetCreditInput =
            serde_json::from_value(source_json.clone()).unwrap();
        assert!(matches!(source, ProviderConsumeResetCreditInput::Source(_)));
        assert_eq!(serde_json::to_value(&source).unwrap(), source_json);
        assert!(serde_json::from_value::<ProviderConsumeResetCreditInput>(json!({})).is_err());
    }

    #[test]
    fn outcome_error_and_report() {
        for (outcome, wire) in [
            (ProviderConsumeResetCreditOutcome::Reset, "reset"),
            (
                ProviderConsumeResetCreditOutcome::NothingToReset,
                "nothingToReset",
            ),
            (ProviderConsumeResetCreditOutcome::NoCredit, "noCredit"),
            (
                ProviderConsumeResetCreditOutcome::AlreadyRedeemed,
                "alreadyRedeemed",
            ),
        ] {
            assert_eq!(serde_json::to_value(outcome).unwrap(), json!(wire));
        }
        let error = UsageLimitSourceError::SourceError {
            detail: "nope".into(),
        };
        assert_eq!(error.to_string(), "nope");
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            json!({"_tag": "UsageLimitSourceError", "detail": "nope"})
        );
        let report_json = json!({
            "createdAt": "2026-01-01T00:00:00Z",
            "accounts": [{
                "id": "a", "driver": "codex", "label": "Codex", "instanceId": "codex",
                "resetCreditInput": {"instanceId": "codex"}, "displayName": "", "accentColor": "#fff",
                "limits": {"checkedAt": "x", "windows": [window("a", 1.0)]},
            }],
            "notices": ["hello"],
        });
        let report: UsageLimitsReport = serde_json::from_value(report_json.clone()).unwrap();
        assert_eq!(serde_json::to_value(&report).unwrap(), report_json);
    }
}
