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

// --- client definition -------------------------------------------------------------------------

use std::collections::HashSet;
use std::sync::{Arc, LazyLock};

use regex::Regex;
use turin_contracts::pull_request::PullRequestAction;
use turin_contracts::source_control::SourceControlProviderKind;

use crate::source_control_core::definition::{
    ChangeRequestTerminology, CloneTransport, SourceControlClientDefinition,
    is_change_request_in_project_repository, is_change_request_on_project_host,
    is_change_request_path,
};

static CHECKOUT_COMMAND: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^gh\s+pr\s+checkout\s+(.+)$").unwrap());
static CHANGE_REQUEST_REFERENCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^https://github\.com/[^/\s]+/[^/\s]+/pull/(\d+)(?:[/?#].*)?$").unwrap()
});

fn github_kind() -> SourceControlProviderKind {
    SourceControlProviderKind::new(KIND).expect("github is a valid kind")
}

/// GitHub's client definition (`definition`).
pub fn definition() -> SourceControlClientDefinition {
    let in_repository_kind = github_kind();
    let on_host_kind = github_kind();
    let host_kind = github_kind();
    SourceControlClientDefinition {
        kind: github_kind(),
        label: "GitHub".into(),
        picker_label: "GitHub".into(),
        icon: "github".into(),
        change_request: ChangeRequestTerminology { short_label: "PR", singular: "pull request" },
        repository_path_hint: "owner/repo".into(),
        public_host: Some("github.com".into()),
        publish_description: "github.com".into(),
        publish_host: Arc::new(|_| "github.com".into()),
        new_repository_owner: Arc::new(|account| Some(account.map(str::to_owned))),
        default_clone_transport: CloneTransport::Https,
        change_request_url: Arc::new(|input| {
            Some(format!("https://{}/{}/pull/{}", input.host, input.repository, input.number))
        }),
        change_request_actions: HashSet::from([
            PullRequestAction::Merge,
            PullRequestAction::Ready,
            PullRequestAction::Draft,
            PullRequestAction::Close,
            PullRequestAction::Reopen,
            PullRequestAction::UpdateBranch,
            PullRequestAction::EnableAutoMerge,
            PullRequestAction::DisableAutoMerge,
            PullRequestAction::Revert,
            PullRequestAction::ApproveWorkflows,
        ]),
        checkout_command: Arc::new(|input| Some(format!("gh pr checkout {}", input.number))),
        author_profile_url: Arc::new(|login, repository_url| {
            if login.ends_with("[bot]") {
                return None;
            }
            reqwest::Url::parse(repository_url)
                .ok()?
                .join(&format!("/{}", crate::shared::js::encode_uri_component(login)))
                .ok()
                .map(|url| url.to_string())
        }),
        reference_autolink_repository_url: Arc::new(|repository_url| Some(repository_url.to_owned())),
        review_summary_required: Arc::new(|_| false),
        checkout_command_argument: Arc::new(|input| {
            CHECKOUT_COMMAND
                .captures(input)
                .and_then(|c| c.get(1))
                .map(|m| crate::shared::js::js_trim(m.as_str()).to_owned())
        }),
        is_change_request_reference: Arc::new(|url| CHANGE_REQUEST_REFERENCE.is_match(url)),
        change_request_url_host: Arc::new(|url| url.host_str().unwrap_or("").to_owned()),
        // A GitHub web host is the checkout's own host, Enterprise installs included.
        checkout_change_request_host: Arc::new(move |identity| {
            Some(turin_contracts::pull_request::pull_request_host_of(
                Some(&turin_contracts::pull_request::RepositoryIdentityLike {
                    canonical_key: Some(identity.canonical_key.as_str()),
                    remote_url: Some(identity.locator.remote_url.as_str()),
                }),
                host_kind.clone(),
            ))
        }),
        is_change_request_in_repository: Arc::new(move |identity, link| {
            is_change_request_in_project_repository(&in_repository_kind, identity, link)
        }),
        can_read_change_request_on_host: Arc::new(move |identity, link| {
            is_change_request_on_project_host(&on_host_kind, identity, link)
        }),
        is_change_request_url: Arc::new(|url| is_change_request_path(url, "/pull/")),
        secret_settings_fields: SECRET_SETTINGS_FIELDS,
    }
}

#[cfg(test)]
mod definition_tests {
    use super::*;
    use crate::source_control_core::definition::{ChangeRequestCheckoutInput, ChangeRequestUrlInput};

    #[test]
    fn builds_github_change_request_urls_and_commands() {
        let d = definition();
        assert_eq!(
            (d.change_request_url)(&ChangeRequestUrlInput {
                host: "github.com".into(),
                repository: "a/b".into(),
                number: 7,
                remote_url: None,
                web_url: None,
            })
            .as_deref(),
            Some("https://github.com/a/b/pull/7")
        );
        assert_eq!(
            (d.checkout_command)(&ChangeRequestCheckoutInput {
                number: 7,
                head_branch: "x".into(),
                head_repository_name_with_owner: None,
                repository_url: None,
            })
            .as_deref(),
            Some("gh pr checkout 7")
        );
        assert_eq!((d.checkout_command_argument)("gh pr checkout  42 ").as_deref(), Some("42"));
        assert_eq!((d.checkout_command_argument)("git checkout x"), None);
        assert!((d.is_change_request_reference)("https://github.com/a/b/pull/12/files"));
        assert!(!(d.is_change_request_reference)("https://ghe.example/a/b/pull/12"));
        assert!((d.is_change_request_url)("https://ghe.example/a/b/pull/12"));
        assert_eq!(
            (d.author_profile_url)("octo cat", "https://github.com/a/b").as_deref(),
            Some("https://github.com/octo%20cat")
        );
        assert_eq!((d.author_profile_url)("dependabot[bot]", "https://github.com/a/b"), None);
        assert_eq!((d.new_repository_owner)(Some("me")), Some(Some("me".to_owned())));
        assert_eq!(d.change_request_actions.len(), 10);
    }
}
