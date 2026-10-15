//! Port of upstream/packages/contracts/src/environment.ts (local fields only;
//! the HTTP/relay surfaces live in the cut `environmentHttp` module).

use crate::base_schemas::{EnvironmentId, PositiveInt, ProjectId, ThreadId, TrimmedNonEmptyString};
use serde::{Deserialize, Serialize};

/// Wire version for orchestration snapshots, streams, commands, and RPC payloads.
pub const ORCHESTRATION_PROTOCOL_VERSION: i64 = 2;
pub const ORCHESTRATION_PROTOCOL_VERSION_TEXT: &str = "2";
pub const ORCHESTRATION_PROTOCOL_QUERY_PARAM: &str = "orchestrationProtocol";
pub const ORCHESTRATION_PROTOCOL_HEADER: &str = "x-t3-orchestration-protocol";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExecutionEnvironmentPlatformOs {
    Darwin,
    Linux,
    Windows,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExecutionEnvironmentPlatformArch {
    Arm64,
    X64,
    Other,
}

/// The curated set of machine shapes and OS identities an environment can
/// wear as its icon. Servers detect one from the hardware they run on
/// (`platform.machine`), and the `environmentIcon` server setting lets a user
/// pick one instead.
pub const ENVIRONMENT_MACHINE_KINDS: &[&str] = &[
    "server",
    "cloud",
    "linux",
    "desktop",
    "laptop",
    "mac-mini",
    "mac-studio",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EnvironmentMachineKind {
    Server,
    Cloud,
    Linux,
    Desktop,
    Laptop,
    MacMini,
    MacStudio,
}

/// Whether a raw string is one of [`ENVIRONMENT_MACHINE_KINDS`]
/// (upstream `isEnvironmentMachineKind`).
pub fn is_environment_machine_kind(value: &str) -> bool {
    ENVIRONMENT_MACHINE_KINDS.contains(&value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionEnvironmentPlatform {
    pub os: ExecutionEnvironmentPlatformOs,
    pub arch: ExecutionEnvironmentPlatformArch,
    /// Hardware shape detected at startup. Absent when the host gives no
    /// usable signal (containers, Windows, unknown DMI), on servers that
    /// predate it, or when a newer server names a kind this build cannot
    /// draw — `ForwardCompatibleOptional`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::forward_compatible_optional"
    )]
    pub machine: Option<EnvironmentMachineKind>,
}

/// Where a new thread runs: the project's current checkout ("local") or a
/// fresh git worktree ("worktree"). Lives here (not settings.ts) so
/// orchestration contracts can reference it without an import cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ThreadEnvMode {
    #[serde(rename = "local")]
    Local,
    #[serde(rename = "worktree")]
    Worktree,
}

/// How a new worktree populates git submodules: every nested level, only the
/// ones this repository declares, or not at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorktreeSubmodules {
    Recursive,
    TopLevel,
    None,
}

/// How a server can replace itself with another version when asked over RPC.
/// New servers only advertise the stable launcher-backed "boot-service" path;
/// "respawn" remains decodable for compatibility with older servers.
/// "desktop-app" means the supervising desktop app updated and relaunched
/// itself, bringing the server back with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServerSelfUpdateMethod {
    BootService,
    Respawn,
    DesktopApp,
}

/// Proven ownership for a manual update; unknown installs omit this descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum ServerInstallation {
    Npx,
    PnpmDlx,
    Bunx,
    NpmGlobal { prefix: TrimmedNonEmptyString },
}

/// What update path a client should offer for a server: one of the RPC
/// self-update methods above, or "desktop-managed" when the backend's version
/// belongs to the T3 Code desktop app supervising it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServerSelfUpdateCapability {
    BootService,
    Respawn,
    DesktopManaged,
}

fn default_false() -> bool {
    false
}

/// Shape of a file-attachment capability (`fileAttachments`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileAttachmentCapability {
    pub max_upload_bytes: PositiveInt,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ExecutionEnvironmentCapabilities {
    #[serde(default = "default_false")]
    pub repository_identity: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_probe: Option<bool>,
    /// Missing on older servers, which still accept inline image attachments.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachment_uploads: Option<bool>,
    /// Uploaded files may accompany question answers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub question_attachments: Option<bool>,
    /// Missing on servers that only accept image attachments.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_attachments: Option<FileAttachmentCapability>,
    /// Server exposes the pull-request list, detail, activity, diff, and
    /// mutation APIs. Absent on servers from before the pull-request
    /// workspace shipped, so clients must not probe them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pull_requests: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pull_request_checks: Option<bool>,
    /// Server understands canonical inline context links plus their message
    /// context records. Absent on servers from before inline context
    /// shipped, which drop the records and forward the links as literal
    /// text — so a client must serialize context the legacy way for them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inline_message_context: Option<bool>,
    /// Server rejects required worktrees instead of falling back to the
    /// project checkout.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_worktree_bootstrap: Option<bool>,
    /// Server understands thread.settle / thread.unsettle commands. Absent
    /// on pre-settlement servers, so clients treat missing as unsupported
    /// and never send the commands under version skew.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_settlement: Option<bool>,
    /// Server evaluates merge and inactivity settlement without a client.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_auto_settlement: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage_cleanup: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage_cleanup_run: Option<bool>,
    /// Server checks OTLP endpoints for the telemetry export settings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub otlp_endpoint_check: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_worktree_cleanup: Option<bool>,
    /// Server honors the `worktreesDirectory` setting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktrees_directory: Option<bool>,
    /// Server can exclude PR-linked threads from inactivity settlement.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_auto_settlement_scope: Option<bool>,
    /// Server persists the opt-in for continuing interrupted threads after
    /// restarts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_restart_continuation: Option<bool>,
    /// Server resolves `projectSettingsOverrides`; older servers ignore the
    /// key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_settings_overrides: Option<bool>,
    /// Server resolves a project's `providerInstances` override entry. Older
    /// servers decode the key away (unknown fields are stripped, not
    /// rejected), so the per-project provider switch would silently do
    /// nothing; clients hide it instead of sending a patch that goes nowhere.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_provider_instance_overrides: Option<bool>,
    /// Server understands thread.snooze / thread.unsnooze commands. Same
    /// version-skew contract as threadSettlement.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_snooze: Option<bool>,
    /// Server streams themes an environment publishes. Absent on servers
    /// from before environment themes shipped, which never emit the events —
    /// so a client reconnecting to one must drop published themes rather
    /// than keep showing a set nothing will ever update.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment_themes: Option<bool>,
    /// Server streams quota from configured usage-limit sources. Same
    /// version-skew contract as environmentThemes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_limit_sources: Option<bool>,
    /// Server persists custom model rates and applies them to usage summaries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_price_overrides: Option<bool>,
    /// Server persists model mappings and folds mapped usage into the target
    /// model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_model_aliases: Option<bool>,
    /// Server understands thread.pin / thread.unpin commands. Same
    /// version-skew contract as threadSettlement.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_pinning: Option<bool>,
    /// Server understands thread.pin.reorder (and orderKey on thread.pin).
    /// Same version-skew contract as threadSettlement.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_pin_reorder: Option<bool>,
    /// Server persists manual Active order through thread.active.reorder.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_active_reorder: Option<bool>,
    /// Server understands thread.auto-settle.set (per-thread auto-settle
    /// off). Same version-skew contract as threadSettlement.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_auto_settle_opt_out: Option<bool>,
    /// Server understands regenerateTitle on thread.meta.update. Absent on
    /// older servers, so clients hide the action instead of sending it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_title_regeneration: Option<bool>,
    /// Server understands thread.visit / thread.mark-unread commands and
    /// projects lastVisitedAt on thread shells. Same version-skew contract
    /// as threadSettlement: clients keep their local visited state against
    /// servers that lack this.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_visited_tracking: Option<bool>,
    /// Server persists a pull request reference on thread.meta.update.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_pull_request_linking: Option<bool>,
    /// Server resolves message delivery and model-selection context and
    /// validates identified rollback readiness. Clients retain
    /// projection-based command shaping and validation when this is absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_resolved_command_context: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_pull_requests: Option<bool>,
    /// Server understands thread.pull-request.watch and wakes agents on pull
    /// request changes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_pull_request_watch: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pull_request_stack_actions: Option<bool>,
    /// The update path clients should offer for this server. Absent on
    /// servers that must be relaunched manually (dev checkouts, Windows
    /// foreground runs, pre-update servers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_self_update: Option<ServerSelfUpdateCapability>,
    /// Manual commands must update this install, not the host's default
    /// global prefix — `ForwardCompatibleOptional`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::forward_compatible_optional"
    )]
    pub server_installation: Option<ServerInstallation>,
    /// Server can stream self-update progress before acknowledging the
    /// restart. Clients fall back to server.updateServer when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_self_update_progress: Option<bool>,
    /// Server can durably mark running provider turns before a self-update
    /// and continue them after the replacement process starts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_update_thread_continuation: Option<bool>,
    /// Agent-activity publishes (push notifications and Live Activities)
    /// currently leave this environment: the publish opt-in is enabled and
    /// the relay link credentials exist. Clients skip seeding a Live Activity
    /// when this is false — no update would ever repaint it. Absent on older
    /// servers, which may still publish, so only an explicit false skips.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_activity_publishing: Option<bool>,
    /// Server runs repository clones for new projects in the background and
    /// streams their progress (`projectClone.*`, `subscribeProjectClones`).
    /// Absent on older servers, where clients must clone with the blocking
    /// `sourceControl.cloneRepository` call instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_clone_tracking: Option<bool>,
    /// Server detects `platform.machine` and persists the `environmentIcon`
    /// setting. Older servers drop the key on write, so clients show the
    /// picker inert rather than offering a choice that would never stick.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment_icon: Option<bool>,
    /// The desktop app supervising this server can be driven over RPC:
    /// server.updateServer runs its check -> download -> relaunch. Absent on
    /// desktop servers whose app predates the remote trigger, where clients
    /// must keep telling the user to update the app on that machine.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desktop_app_update: Option<bool>,
    /// Server hosts preview tabs in its own headless Chromium
    /// (`runtime: "server"`) and streams them over `/api/preview-stream`.
    /// Clients without a local browser runtime open server tabs here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_browser: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionEnvironmentDescriptor {
    pub environment_id: EnvironmentId,
    pub label: TrimmedNonEmptyString,
    pub platform: ExecutionEnvironmentPlatform,
    pub server_version: TrimmedNonEmptyString,
    /// Absent on hosts from before explicit orchestration protocol negotiation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestration_protocol_version: Option<i64>,
    pub capabilities: ExecutionEnvironmentCapabilities,
}

string_literal!(
    /// Constant `"git-remote"` source of a repository identity locator.
    GitRemoteSource,
    "git-remote"
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryIdentityLocator {
    pub source: GitRemoteSource,
    pub remote_name: TrimmedNonEmptyString,
    pub remote_url: TrimmedNonEmptyString,
}

/// The checkout's own remote when it names a different repository than the
/// canonical one, such as a fork that tracks its upstream. Clients group and
/// label by it so a fork stays distinct from the repository it forked, while
/// pull request features keep the canonical identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryOrigin {
    pub canonical_key: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<TrimmedNonEmptyString>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryIdentity {
    pub canonical_key: TrimmedNonEmptyString,
    pub locator: RepositoryIdentityLocator,
    /// Repository browser URL resolved from the server's configured hosting account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_url: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_path: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<RepositoryOrigin>,
}

/// Key clients group checkouts by: a fork's own remote, otherwise the
/// canonical repository.
pub fn repository_grouping_key_of(identity: &RepositoryIdentity) -> &str {
    identity
        .origin
        .as_ref()
        .map(|origin| origin.canonical_key.as_str())
        .unwrap_or(identity.canonical_key.as_str())
}

/// Label clients show for a checkout's repository, matching
/// [`repository_grouping_key_of`].
pub fn repository_grouping_display_name_of(identity: &RepositoryIdentity) -> Option<&str> {
    match &identity.origin {
        Some(origin) => Some(
            origin
                .display_name
                .as_ref()
                .map(|name| name.as_str())
                .unwrap_or(origin.canonical_key.as_str()),
        ),
        None => identity.display_name.as_ref().map(|name| name.as_str()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopedProjectRef {
    pub environment_id: EnvironmentId,
    pub project_id: ProjectId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopedThreadRef {
    pub environment_id: EnvironmentId,
    pub thread_id: ThreadId,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn descriptor(capabilities: Value) -> Value {
        json!({
            "environmentId": "environment-1",
            "label": "Local",
            "platform": { "os": "darwin", "arch": "arm64" },
            "serverVersion": "0.0.32",
            "capabilities": capabilities,
        })
    }

    fn decode(capabilities: Value) -> ExecutionEnvironmentDescriptor {
        serde_json::from_value(descriptor(capabilities)).unwrap()
    }

    #[test]
    fn manual_installation_descriptors_old_recognized_and_future() {
        assert_eq!(
            decode(json!({ "repositoryIdentity": true }))
                .capabilities
                .server_installation,
            None
        );
        for installation in [
            json!({ "kind": "npx" }),
            json!({ "kind": "npm-global", "prefix": "/opt/node" }),
        ] {
            let decoded =
                decode(json!({ "repositoryIdentity": true, "serverInstallation": installation }));
            let encoded =
                serde_json::to_value(decoded.capabilities.server_installation.unwrap()).unwrap();
            assert_eq!(encoded, installation);
        }
        for installation in [
            json!({ "kind": "future-manager" }),
            json!({ "kind": "npm-global" }),
        ] {
            let decoded =
                decode(json!({ "repositoryIdentity": true, "serverInstallation": installation }));
            assert_eq!(decoded.capabilities.server_installation, None);
        }
    }

    #[test]
    fn optional_capabilities_are_unsupported_when_missing_and_preserved_when_advertised() {
        let bare = decode(json!({ "repositoryIdentity": true })).capabilities;
        assert_eq!(bare.required_worktree_bootstrap, None);
        assert_eq!(bare.pull_requests, None);
        assert_eq!(bare.attachment_uploads, None);
        assert_eq!(bare.server_resolved_command_context, None);
        let advertised = decode(json!({
            "repositoryIdentity": true, "requiredWorktreeBootstrap": true, "pullRequests": true,
            "attachmentUploads": true, "serverResolvedCommandContext": true,
            "fileAttachments": { "maxUploadBytes": 50 * 1024 * 1024 },
        }))
        .capabilities;
        assert_eq!(advertised.required_worktree_bootstrap, Some(true));
        assert_eq!(advertised.pull_requests, Some(true));
        assert_eq!(advertised.attachment_uploads, Some(true));
        assert_eq!(advertised.server_resolved_command_context, Some(true));
        assert_eq!(
            advertised.file_attachments.unwrap().max_upload_bytes.get(),
            50 * 1024 * 1024
        );
    }

    #[test]
    fn repository_identity_defaults_to_false_and_is_always_encoded() {
        let capabilities = decode(json!({})).capabilities;
        assert!(!capabilities.repository_identity);
        assert_eq!(
            serde_json::to_value(&capabilities).unwrap(),
            json!({ "repositoryIdentity": false })
        );
        assert!(
            serde_json::from_value::<ExecutionEnvironmentCapabilities>(
                json!({ "fileAttachments": { "maxUploadBytes": 0 } })
            )
            .is_err()
        );
    }

    #[test]
    fn machine_kind_is_forward_compatible_optional() {
        let known: ExecutionEnvironmentPlatform =
            serde_json::from_value(json!({ "os": "linux", "arch": "x64", "machine": "mac-mini" }))
                .unwrap();
        assert_eq!(known.machine, Some(EnvironmentMachineKind::MacMini));
        let future: ExecutionEnvironmentPlatform =
            serde_json::from_value(json!({ "os": "linux", "arch": "x64", "machine": "quantum" }))
                .unwrap();
        assert_eq!(future.machine, None);
        assert_eq!(
            serde_json::to_value(future).unwrap(),
            json!({ "os": "linux", "arch": "x64" })
        );
        assert!(
            ENVIRONMENT_MACHINE_KINDS
                .iter()
                .all(|kind| is_environment_machine_kind(kind))
        );
        for kind in ENVIRONMENT_MACHINE_KINDS {
            let parsed: EnvironmentMachineKind = serde_json::from_value(json!(kind)).unwrap();
            assert_eq!(serde_json::to_value(parsed).unwrap(), json!(kind));
        }
    }

    #[test]
    fn repository_identity_helpers() {
        let identity: RepositoryIdentity = serde_json::from_value(json!({
            "canonicalKey": "github.com/a/b",
            "locator": { "source": "git-remote", "remoteName": "origin", "remoteUrl": "git@github.com:a/b.git" },
            "displayName": "a/b",
            "origin": { "canonicalKey": "github.com/me/b" },
        }))
        .unwrap();
        assert_eq!(repository_grouping_key_of(&identity), "github.com/me/b");
        assert_eq!(
            repository_grouping_display_name_of(&identity),
            Some("github.com/me/b")
        );
        let plain: RepositoryIdentity = serde_json::from_value(json!({
            "canonicalKey": "github.com/a/b",
            "locator": { "source": "git-remote", "remoteName": "origin", "remoteUrl": "u" },
        }))
        .unwrap();
        assert_eq!(repository_grouping_key_of(&plain), "github.com/a/b");
        assert_eq!(repository_grouping_display_name_of(&plain), None);
    }

    #[test]
    fn submodule_and_env_mode_literals() {
        for (mode, wire) in [
            (WorktreeSubmodules::Recursive, "recursive"),
            (WorktreeSubmodules::TopLevel, "top-level"),
            (WorktreeSubmodules::None, "none"),
        ] {
            assert_eq!(serde_json::to_value(mode).unwrap(), json!(wire));
        }
        assert_eq!(
            serde_json::to_value(ThreadEnvMode::Worktree).unwrap(),
            json!("worktree")
        );
        assert_eq!(
            serde_json::to_value(ServerSelfUpdateCapability::DesktopManaged).unwrap(),
            json!("desktop-managed")
        );
    }
}
