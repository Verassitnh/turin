//! Port of upstream/apps/web/src/components/settings/GitHubAccountSettings.logic.ts
//! (and its test file).

use indexmap::IndexMap;
use turin_contracts::source_control::SourceControlProviderAccount;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubHostGroup {
    pub host: String,
    /// gh's active stored login, the one used when Settings pin nothing.
    pub active_account: Option<String>,
    /// Stored logins that work and can be pinned with `gh auth token --user`.
    pub selectable: Vec<String>,
    /// Logins gh holds but cannot use; shown with their error.
    pub broken: Vec<SourceControlProviderAccount>,
    /// Set when GH_TOKEN or a sibling overrides every stored login for this host.
    pub environment_variable: Option<String>,
}

/// Groups gh's logins by host, in the order gh reported the hosts.
pub fn group_github_accounts(accounts: &[SourceControlProviderAccount]) -> Vec<GitHubHostGroup> {
    let mut hosts: Vec<&str> = Vec::new();
    for entry in accounts {
        if !hosts.contains(&entry.host.as_str()) {
            hosts.push(entry.host.as_str());
        }
    }
    hosts
        .into_iter()
        .map(|host| {
            let entries: Vec<&SourceControlProviderAccount> =
                accounts.iter().filter(|entry| entry.host.as_str() == host).collect();
            let stored: Vec<&&SourceControlProviderAccount> = entries
                .iter()
                .filter(|entry| entry.environment_variable.is_none())
                .collect();
            let selectable: Vec<&&SourceControlProviderAccount> =
                stored.iter().copied().filter(|entry| entry.authenticated).collect();
            GitHubHostGroup {
                host: host.to_owned(),
                active_account: selectable
                    .iter()
                    .find(|entry| entry.active)
                    .or_else(|| selectable.first())
                    .map(|entry| entry.account.as_str().to_owned()),
                selectable: selectable
                    .iter()
                    .map(|entry| entry.account.as_str().to_owned())
                    .collect(),
                broken: stored
                    .iter()
                    .filter(|entry| !entry.authenticated)
                    .map(|entry| (***entry).clone())
                    .collect(),
                environment_variable: entries
                    .iter()
                    .find_map(|entry| entry.environment_variable.as_ref())
                    .map(|variable| variable.as_str().to_owned()),
            }
        })
        .collect()
}

/// One host's saved choice (`GitHubSettings["hosts"][host]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubHostChoice {
    pub enabled: bool,
    pub account: Option<String>,
}

pub type GitHubHosts = IndexMap<String, GitHubHostChoice>;

/// How one host's choice changes. `account: Some(None)` clears the pin.
#[derive(Debug, Clone, Default)]
pub struct GitHubHostChange {
    pub enabled: Option<bool>,
    pub account: Option<Option<String>>,
}

/// The full `hosts` map after one host changes. Hosts left on gh's defaults (enabled,
/// no pinned account) are dropped so settings only hold real choices.
pub fn next_github_hosts(current: &GitHubHosts, host: &str, change: GitHubHostChange) -> GitHubHosts {
    let previous = current.get(host);
    let enabled = change
        .enabled
        .or_else(|| previous.map(|previous| previous.enabled))
        .unwrap_or(true);
    let account = match change.account {
        None => previous.and_then(|previous| previous.account.clone()),
        Some(account) => account,
    };
    let mut rest = current.clone();
    rest.shift_remove(host);
    if enabled && account.is_none() {
        return rest;
    }
    rest.insert(host.to_owned(), GitHubHostChoice { enabled, account });
    rest
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    fn account(value: serde_json::Value) -> SourceControlProviderAccount {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn groups_logins_by_host_keeping_broken_and_environment_logins_out_of_the_picker() {
        let old = account(
            json!({ "host": "github.com", "account": "old", "active": false, "authenticated": false, "error": "bad" }),
        );
        let groups = group_github_accounts(&[
            account(json!({ "host": "github.com", "account": "personal", "active": true, "authenticated": true })),
            account(json!({ "host": "github.com", "account": "work", "active": false, "authenticated": true })),
            old.clone(),
            account(json!({ "host": "github.com", "account": "bot", "active": false, "authenticated": true, "environmentVariable": "GH_TOKEN" })),
            account(json!({ "host": "ghe.acme.test", "account": "jm", "active": false, "authenticated": true })),
        ]);
        assert_eq!(
            groups,
            vec![
                GitHubHostGroup {
                    host: "github.com".into(),
                    active_account: Some("personal".into()),
                    selectable: vec!["personal".into(), "work".into()],
                    broken: vec![old],
                    environment_variable: Some("GH_TOKEN".into()),
                },
                GitHubHostGroup {
                    host: "ghe.acme.test".into(),
                    active_account: Some("jm".into()),
                    selectable: vec!["jm".into()],
                    broken: vec![],
                    environment_variable: None,
                },
            ]
        );
    }

    #[test]
    fn next_github_hosts_pins_an_account_keeps_other_hosts_and_drops_a_host_back_on_gh_defaults() {
        let mut other = GitHubHosts::new();
        other.insert(
            "ghe.acme.test".into(),
            GitHubHostChoice {
                enabled: false,
                account: None,
            },
        );
        let pinned = next_github_hosts(
            &other,
            "github.com",
            GitHubHostChange {
                account: Some(Some("work".into())),
                ..Default::default()
            },
        );
        let mut expected = other.clone();
        expected.insert(
            "github.com".into(),
            GitHubHostChoice {
                enabled: true,
                account: Some("work".into()),
            },
        );
        assert_eq!(pinned, expected);

        let disabled = next_github_hosts(
            &pinned,
            "github.com",
            GitHubHostChange {
                enabled: Some(false),
                ..Default::default()
            },
        );
        assert_eq!(
            disabled.get("github.com"),
            Some(&GitHubHostChoice {
                enabled: false,
                account: Some("work".into())
            })
        );

        assert_eq!(
            next_github_hosts(
                &disabled,
                "github.com",
                GitHubHostChange {
                    enabled: Some(true),
                    account: Some(None)
                }
            ),
            other
        );
    }
}
