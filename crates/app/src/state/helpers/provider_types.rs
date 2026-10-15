//! The slice of upstream's `ServerProvider` (packages/contracts/src/server.ts) the helpers read.
//!
//! The real type is `turin_provider_core::server_provider::ServerProvider` (T08), a crate the
//! app does not depend on yet. This mirrors the fields the helpers use with the same names and
//! optionality; swap it for that type once the dependency exists. Text fields that are
//! `TrimmedNonEmptyString` upstream are `String` here unless a helper builds a contract value
//! from them.

use turin_contracts::TrimmedNonEmptyString;
use turin_contracts::provider_instance::{ProviderDriverKind, ProviderInstanceId};

use crate::state::support::model::SelectableModelOption;

/// `ServerProviderState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerProviderState {
    Ready,
    Warning,
    Error,
    Disabled,
}

/// `ServerProviderAvailability`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerProviderAvailability {
    Available,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerProviderModel {
    pub slug: TrimmedNonEmptyString,
    pub name: TrimmedNonEmptyString,
    pub aliases: Option<Vec<String>>,
    pub is_default: Option<bool>,
}

impl SelectableModelOption for ServerProviderModel {
    fn slug(&self) -> &str {
        self.slug.as_str()
    }
    fn name(&self) -> &str {
        self.name.as_str()
    }
    fn aliases(&self) -> Option<&[String]> {
        self.aliases.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerProviderUpdateRequiredModel {
    pub slug: String,
    pub name: String,
    pub min_version: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerProviderSlashCommand {
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerProviderSkill {
    pub name: String,
    pub path: String,
    pub scope: Option<String>,
    pub enabled: bool,
    pub display_name: Option<String>,
    pub user_invocation_only: Option<bool>,
    pub user_invocable: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerProviderWorkspaceSnapshot {
    pub cwd: String,
    pub checked_at: String,
    pub slash_commands: Vec<ServerProviderSlashCommand>,
    /// Skills are available, but command discovery still needs a retry.
    pub slash_commands_pending: Option<bool>,
    pub skills: Vec<ServerProviderSkill>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerProviderContinuation {
    pub group_key: String,
}

/// How long a workspace's skill and command scan stays current. Nothing watches
/// skill directories, so a composer opened after this rescans on use, and the
/// server answers repeat requests inside the window from its cache.
pub const PROVIDER_WORKSPACE_SNAPSHOT_TTL_MS: i64 = 5 * 60_000;

/// `Date.parse` for the timestamps the server writes (RFC 3339), plus a bare ISO date. `None`
/// stands for `NaN`.
pub fn parse_date_ms(value: &str) -> Option<i64> {
    if let Ok(date) = chrono::DateTime::parse_from_rfc3339(value) {
        return Some(date.timestamp_millis());
    }
    chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|date| date.and_utc().timestamp_millis())
}

pub fn is_provider_workspace_snapshot_current(
    snapshot: &ServerProviderWorkspaceSnapshot,
    now_ms: i64,
) -> bool {
    // `NaN < x` is false, so an unparseable timestamp is never current.
    parse_date_ms(&snapshot.checked_at)
        .is_some_and(|checked_ms| now_ms - checked_ms < PROVIDER_WORKSPACE_SNAPSHOT_TTL_MS)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerProvider {
    pub instance_id: ProviderInstanceId,
    pub driver: ProviderDriverKind,
    pub enabled: bool,
    pub status: ServerProviderState,
    pub availability: Option<ServerProviderAvailability>,
    pub continuation: Option<ServerProviderContinuation>,
    pub requires_new_thread_for_model_change: Option<bool>,
    pub models: Vec<ServerProviderModel>,
    pub update_required_models: Option<Vec<ServerProviderUpdateRequiredModel>>,
    pub slash_commands: Vec<ServerProviderSlashCommand>,
    pub skills: Vec<ServerProviderSkill>,
    pub workspace_snapshots: Option<Vec<ServerProviderWorkspaceSnapshot>>,
}

impl ServerProvider {
    /// A ready, enabled provider with nothing else set; callers fill in what they read.
    pub fn new(instance_id: ProviderInstanceId, driver: ProviderDriverKind) -> Self {
        Self {
            instance_id,
            driver,
            enabled: true,
            status: ServerProviderState::Ready,
            availability: None,
            continuation: None,
            requires_new_thread_for_model_change: None,
            models: Vec::new(),
            update_required_models: None,
            slash_commands: Vec::new(),
            skills: Vec::new(),
            workspace_snapshots: None,
        }
    }
}
