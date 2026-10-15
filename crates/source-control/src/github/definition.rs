//! Port of upstream/packages/source-control-github/src/client/definition.ts
//!
//! (Settings half. The client definition value — labels, URL builders, checkout command — is
//! added with `source_control_core::definition`.)

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use turin_contracts::settings::GitHubHost;
use turin_contracts::{TrimmedNonEmptyString, TrimmedString};

pub const KIND: &str = "github";

fn default_enabled() -> bool {
    true
}

/// One host's choice, keyed by lowercased host such as `github.com`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubHostChoice {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<TrimmedNonEmptyString>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

impl Default for GitHubHostChoice {
    fn default() -> Self {
        Self { account: None, enabled: true }
    }
}

/// Per-host choices, keyed by lowercased host such as `github.com`. `hosts` pins one of the
/// logins `gh` holds for a host instead of its active one, or turns the host off; `tokens` holds a
/// token per host, used before `GH_TOKEN` and friends, which win over `gh`. Source Control
/// settings draws both with GitHub's own panels, since the logins come from `gh` at runtime.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubSettings {
    #[serde(default)]
    pub hosts: IndexMap<GitHubHost, GitHubHostChoice>,
    /// `secretSetting`: kept in the app's secret store, redacted for clients.
    #[serde(default)]
    pub tokens: IndexMap<GitHubHost, TrimmedString>,
}

impl GitHubSettings {
    /// The Settings choice for a (lowercased) host.
    pub fn host_choice(&self, host: &str) -> Option<&GitHubHostChoice> {
        GitHubHost::new(host).ok().and_then(|key| self.hosts.get(&key))
    }

    /// The trimmed token saved for a (lowercased) host; empty counts as none.
    pub fn token(&self, host: &str) -> Option<&str> {
        let key = GitHubHost::new(host).ok()?;
        self.tokens.get(&key).map(|t| t.as_str()).filter(|t| !t.is_empty())
    }
}

/// The secret fields of the settings struct, as `secretSettingsFields(settings)` reports them.
pub const SECRET_SETTINGS_FIELDS: &[&str] = &["tokens"];

/// A host's saved settings decoded with its schema. A blob that no longer decodes, such as one a
/// newer build wrote, reads as the schema's defaults rather than failing the host
/// (`readSourceControlHostSettings`).
pub fn read_settings(saved: Option<&serde_json::Value>) -> GitHubSettings {
    saved
        .and_then(|value| serde_json::from_value::<GitHubSettings>(value.clone()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_or_undecodable_blobs_read_as_defaults() {
        assert_eq!(read_settings(None), GitHubSettings::default());
        assert_eq!(read_settings(Some(&json!("nope"))), GitHubSettings::default());
        assert_eq!(read_settings(Some(&json!({ "hosts": { "github.com": { "enabled": "x" } } }))), GitHubSettings::default());
    }

    #[test]
    fn hosts_are_lowercased_and_enabled_defaults_true() {
        let s = read_settings(Some(&json!({
            "hosts": { "GitHub.com": { "account": "octocat" }, "ghe.example.com": { "enabled": false } },
            "tokens": { "github.com": "  abc  " }
        })));
        let choice = s.host_choice("github.com").unwrap();
        assert!(choice.enabled);
        assert_eq!(choice.account.as_ref().unwrap().as_str(), "octocat");
        assert!(!s.host_choice("ghe.example.com").unwrap().enabled);
        assert_eq!(s.token("github.com"), Some("abc"));
        assert_eq!(s.token("nope.com"), None);
    }
}
