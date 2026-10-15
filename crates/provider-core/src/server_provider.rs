//! Provider status snapshot types.
//!
//! Port of the provider part of upstream/packages/contracts/src/server.ts
//! (`ServerProvider` and friends). `turin-contracts` (T01) did not carry these
//! even though the kept provider layer is built on them, so they live here
//! until contracts grows a `server` module; the shapes are the wire shapes.
//!
//! Cut: `ServerProviderAuth.action` (an `AcpRegistryUrlAuthAction`; ACP
//! registry is cut).

use std::ops::{Deref, DerefMut};

use serde::{Deserialize, Serialize};
use turin_contracts::{
    IsoDateTime, TrimmedNonEmptyString,
    model::ModelCapabilities,
    provider_instance::{ProviderDriverKind, ProviderInstanceId},
    provider_policy::RuntimeMode,
    provider_usage_limits::ServerProviderUsageLimits,
};

type Tne = TrimmedNonEmptyString;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServerProviderState {
    Ready,
    Warning,
    Error,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServerProviderAuthStatus {
    Authenticated,
    Unauthenticated,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderAuth {
    pub status: ServerProviderAuthStatus,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub type_: Option<Tne>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<Tne>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<Tne>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_logout: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_sharing: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<Tne>,
    /// The workspace or organization whose quota the login draws on, such as a
    /// ChatGPT workspace or Claude organization id. One email can belong to
    /// several, each with its own quota.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<Tne>,
}

impl ServerProviderAuth {
    pub fn with_status(status: ServerProviderAuthStatus) -> Self {
        Self {
            status,
            type_: None,
            label: None,
            email: None,
            can_logout: None,
            subscription_sharing: None,
            profile_id: None,
            workspace_id: None,
        }
    }
}

/// `badge: Schema.Literal("new")`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ServerProviderModelBadge {
    #[serde(rename = "new")]
    New,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderModel {
    pub slug: Tne,
    pub name: Tne,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_name: Option<Tne>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_provider: Option<Tne>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aliases: Option<Vec<Tne>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge: Option<ServerProviderModelBadge>,
    pub is_custom: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_default: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_legacy: Option<bool>,
    pub capabilities: Option<ModelCapabilities>,
}

/// A model the model manifest announces that the installed provider version is
/// too old to run. It is never selectable; clients show it so users learn that
/// updating the provider unlocks it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderUpdateRequiredModel {
    pub slug: Tne,
    pub name: Tne,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge: Option<ServerProviderModelBadge>,
    pub min_version: Tne,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderSlashCommandInput {
    pub hint: Tne,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderSlashCommand {
    pub name: Tne,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Tne>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<ServerProviderSlashCommandInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderSkill {
    pub name: Tne,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Tne>,
    pub path: Tne,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Tne>,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<Tne>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_description: Option<Tne>,
    /// The skill is hidden from the agent's own skill tool, so only the user can
    /// start it (Claude Code's `disable-model-invocation`). Composers must offer
    /// it as a slash command; naming it in prose does nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_invocation_only: Option<bool>,
    /// The mirror of `user_invocation_only`: Claude Code's `user-invocable:
    /// false` keeps the skill out of its own slash commands, so only the agent
    /// can start it. Composers must not offer it under `/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_invocable: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderWorkspaceSnapshot {
    pub cwd: Tne,
    pub checked_at: IsoDateTime,
    pub slash_commands: Vec<ServerProviderSlashCommand>,
    /// Skills are available, but command discovery still needs a retry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slash_commands_pending: Option<bool>,
    pub skills: Vec<ServerProviderSkill>,
}

/// How long a workspace's skill and command scan stays current. Nothing watches
/// skill directories, so a composer opened after this rescans on use, and the
/// server answers repeat requests inside the window from its cache.
pub const PROVIDER_WORKSPACE_SNAPSHOT_TTL_MS: i64 = 5 * 60_000;

pub fn is_provider_workspace_snapshot_current(checked_at: &str, now_ms: i64) -> bool {
    let checked_ms = chrono::DateTime::parse_from_rfc3339(checked_at)
        .map(|date| date.timestamp_millis())
        .unwrap_or(i64::MIN);
    // `Date.parse` of garbage is NaN and `NaN < x` is false.
    checked_ms != i64::MIN && now_ms - checked_ms < PROVIDER_WORKSPACE_SNAPSHOT_TTL_MS
}

/// Availability of a configured provider instance from the runtime's POV.
///
///  - `available`: the build ships this driver and an instance is wired up.
///  - `unavailable`: settings (or a persisted binding) reference a driver this
///    build doesn't ship. The snapshot is preserved so the UI can render
///    "missing driver" affordances and the data round-trips.
///
/// Snapshots with `availability: "unavailable"` MUST set `installed: false`
/// and `enabled: false`; the runtime refuses turn starts against them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServerProviderAvailability {
    Available,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderContinuation {
    pub group_key: Tne,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServerProviderCompatibilityStatus {
    Unknown,
    Supported,
    Graceful,
    Unsupported,
    Broken,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderCompatibilityAdvisory {
    pub status: ServerProviderCompatibilityStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_version_status: Option<ServerProviderCompatibilityStatus>,
    pub message: Option<Tne>,
    pub recommended_version: Option<Tne>,
    pub recommended_range: Option<Tne>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerProviderVersionAdvisoryStatus {
    Unknown,
    Current,
    BehindLatest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderVersionAdvisory {
    pub status: ServerProviderVersionAdvisoryStatus,
    pub current_version: Option<Tne>,
    pub latest_version: Option<Tne>,
    pub update_command: Option<Tne>,
    /// `withDecodingDefault(false)`; always encoded.
    #[serde(default)]
    pub can_update: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_install_version: Option<bool>,
    pub checked_at: Option<IsoDateTime>,
    pub message: Option<Tne>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServerProviderUpdateStatus {
    Idle,
    Queued,
    Running,
    Succeeded,
    Failed,
    Unchanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderUpdateState {
    pub status: ServerProviderUpdateStatus,
    pub started_at: Option<IsoDateTime>,
    pub finished_at: Option<IsoDateTime>,
    pub message: Option<Tne>,
    /// At most 10,000 characters.
    pub output: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderSetup {
    pub can_authenticate: bool,
    pub can_install: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documentation_url: Option<Tne>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderNativeSessions {
    pub can_list: bool,
    pub can_load: bool,
    pub can_resume: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_delete: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderRuntimePaths {
    pub home_path: Tne,
    pub shadow_home_path: Option<Tne>,
}

/// `ForwardCompatibleArray(RuntimeMode)` as an optional key.
mod runtime_modes {
    use super::RuntimeMode;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        value: &Option<Vec<RuntimeMode>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(modes) => turin_contracts::forward_compatible_array::serialize(modes, s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<Vec<RuntimeMode>>, D::Error> {
        let raw = Option::<Vec<serde_json::Value>>::deserialize(d)?;
        Ok(raw.map(|values| {
            values
                .into_iter()
                .filter_map(|value| serde_json::from_value(value).ok())
                .collect()
        }))
    }
}

fn default_empty_vec<T>() -> Vec<T> {
    Vec::new()
}

/// Everything in [`ServerProvider`] except the instance identity. Drivers
/// build snapshots without knowing their own instance; the identity is
/// stamped on afterwards (upstream `ServerProviderDraft`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderDraft {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<Tne>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent_color: Option<Tne>,
    /// Optional visual identity supplied by the owning provider driver (at most
    /// 2,048 characters). Clients validate remote URLs against that driver's
    /// trusted origin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<Tne>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge_label: Option<Tne>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<ServerProviderContinuation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_interaction_mode_toggle: Option<bool>,
    /// The driver streams context window usage, so a started thread will have a
    /// meter once its activities load. Clients reserve the meter's space on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reports_context_window: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "runtime_modes"
    )]
    pub supported_runtime_modes: Option<Vec<RuntimeMode>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_new_thread_for_model_change: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_conversation_rollback: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_text_generation: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<ServerProviderSetup>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_sessions: Option<ServerProviderNativeSessions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configurable_providers: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_paths: Option<ServerProviderRuntimePaths>,
    pub enabled: bool,
    pub installed: bool,
    pub version: Option<Tne>,
    pub status: ServerProviderState,
    pub auth: ServerProviderAuth,
    pub checked_at: IsoDateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<Tne>,
    /// Absent is read as `available` (see [`ServerProvider::is_available`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability: Option<ServerProviderAvailability>,
    /// Human-readable reason populated when `availability == unavailable`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<Tne>,
    pub models: Vec<ServerProviderModel>,
    /// Kept apart from `models` so clients that predate it never offer them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_required_models: Option<Vec<ServerProviderUpdateRequiredModel>>,
    #[serde(default = "default_empty_vec")]
    pub slash_commands: Vec<ServerProviderSlashCommand>,
    #[serde(default = "default_empty_vec")]
    pub skills: Vec<ServerProviderSkill>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_snapshots: Option<Vec<ServerProviderWorkspaceSnapshot>>,
    /// Absent when the driver has no notion of subscription usage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_limits: Option<ServerProviderUsageLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_advisory: Option<ServerProviderVersionAdvisory>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compatibility_advisory: Option<ServerProviderCompatibilityAdvisory>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_state: Option<ServerProviderUpdateState>,
}

/// One provider instance's status snapshot. `Deref`s to the draft, so
/// `snapshot.enabled`, `snapshot.models`, … read as upstream's flat struct.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProvider {
    /// Routing key for the configured instance this snapshot represents. This
    /// is the only stable identity consumers may use for provider routing.
    pub instance_id: ProviderInstanceId,
    /// Open driver kind slug that selects the implementation handling this
    /// instance. It is metadata/capability context, not a routing key.
    pub driver: ProviderDriverKind,
    #[serde(flatten)]
    pub draft: ServerProviderDraft,
}

impl Deref for ServerProvider {
    type Target = ServerProviderDraft;
    fn deref(&self) -> &ServerProviderDraft {
        &self.draft
    }
}

impl DerefMut for ServerProvider {
    fn deref_mut(&mut self) -> &mut ServerProviderDraft {
        &mut self.draft
    }
}

impl ServerProvider {
    /// Treat the optional `availability` as "available" when absent
    /// (upstream `isProviderAvailable`).
    pub fn is_available(&self) -> bool {
        self.draft.availability != Some(ServerProviderAvailability::Unavailable)
    }

    /// Treat an absent `supportsTextGeneration` as supported
    /// (upstream `isProviderTextGenerationCapable`).
    pub fn is_text_generation_capable(&self) -> bool {
        self.draft.supports_text_generation != Some(false)
    }
}

/// `ServerProvider & Pick<ServerProviderWorkspaceSnapshot, "slashCommandsPending">`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderWorkspaceSnapshot {
    #[serde(flatten)]
    pub provider: ServerProvider,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slash_commands_pending: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snapshot_json() -> serde_json::Value {
        json!({
            "instanceId": "codex",
            "driver": "codex",
            "displayName": "Codex",
            "enabled": true,
            "installed": true,
            "version": "1.2.3",
            "status": "ready",
            "auth": {"status": "authenticated", "type": "chatgpt"},
            "checkedAt": "2026-01-01T00:00:00.000Z",
            "models": [{"slug": "gpt", "name": "GPT", "isCustom": false, "capabilities": null}],
            "slashCommands": [],
            "skills": [],
        })
    }

    #[test]
    fn round_trips_in_upstream_key_order() {
        let provider: ServerProvider = serde_json::from_value(snapshot_json()).unwrap();
        assert!(provider.enabled);
        assert!(provider.is_available());
        assert!(provider.is_text_generation_capable());
        let encoded = serde_json::to_value(&provider).unwrap();
        assert_eq!(encoded, snapshot_json());
        let keys: Vec<&String> = encoded.as_object().unwrap().keys().collect();
        assert_eq!(keys[..3], ["instanceId", "driver", "displayName"]);
    }

    #[test]
    fn defaults_slash_commands_and_skills_and_drops_unknown_runtime_modes() {
        let mut value = snapshot_json();
        value.as_object_mut().unwrap().remove("slashCommands");
        value.as_object_mut().unwrap().remove("skills");
        value["supportedRuntimeModes"] = json!(["full-access", "from-the-future"]);
        let provider: ServerProvider = serde_json::from_value(value).unwrap();
        assert!(provider.slash_commands.is_empty() && provider.skills.is_empty());
        assert_eq!(
            provider.supported_runtime_modes,
            Some(vec![RuntimeMode::FullAccess])
        );
        let encoded = serde_json::to_value(&provider).unwrap();
        assert_eq!(encoded["slashCommands"], json!([]));
        assert_eq!(encoded["supportedRuntimeModes"], json!(["full-access"]));
    }

    #[test]
    fn unavailable_and_text_generation_flags() {
        let mut value = snapshot_json();
        value["availability"] = json!("unavailable");
        value["supportsTextGeneration"] = json!(false);
        let provider: ServerProvider = serde_json::from_value(value).unwrap();
        assert!(!provider.is_available());
        assert!(!provider.is_text_generation_capable());
    }

    #[test]
    fn workspace_snapshot_freshness_window() {
        let checked = "2026-01-01T00:00:00.000Z";
        let base = chrono::DateTime::parse_from_rfc3339(checked)
            .unwrap()
            .timestamp_millis();
        assert!(is_provider_workspace_snapshot_current(
            checked,
            base + 299_999
        ));
        assert!(!is_provider_workspace_snapshot_current(
            checked,
            base + 300_000
        ));
        assert!(!is_provider_workspace_snapshot_current("not a date", base));
    }

    #[test]
    fn version_advisory_can_update_defaults_false() {
        let advisory: ServerProviderVersionAdvisory = serde_json::from_value(json!({
            "status": "behind_latest",
            "currentVersion": "1.0.0",
            "latestVersion": "1.1.0",
            "updateCommand": null,
            "checkedAt": null,
            "message": null,
        }))
        .unwrap();
        assert!(!advisory.can_update);
        assert_eq!(
            serde_json::to_value(&advisory).unwrap()["canUpdate"],
            json!(false)
        );
    }
}
