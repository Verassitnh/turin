//! Port of upstream/packages/source-control-github/src/server/gitHubAuthStatus.ts
//!
//! Reads `gh auth status --json hosts`.

use std::sync::LazyLock;

use indexmap::IndexMap;
use regex::Regex;
use serde::{Deserialize, Deserializer};

use super::definition::GitHubSettings;
use crate::shared::js::js_trim;

/// An optional string key that may be absent but, like `Schema.optional(Schema.String)`, may not
/// be `null` or another type.
fn optional_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    String::deserialize(d).map(Some)
}

#[derive(Deserialize)]
struct AccountJson {
    state: String,
    #[serde(default, deserialize_with = "optional_string")]
    error: Option<String>,
    active: bool,
    host: String,
    login: String,
    #[serde(default, rename = "tokenSource", deserialize_with = "optional_string")]
    token_source: Option<String>,
}

#[derive(Deserialize)]
struct StatusJson {
    hosts: IndexMap<String, Vec<AccountJson>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubAuthStatusAccount {
    pub host: String,
    pub account: String,
    pub authenticated: bool,
    pub active: bool,
    pub error: Option<String>,
    /// The variable (GH_TOKEN and kin) the login came from; it overrides every stored login.
    pub environment_variable: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubAuthStatus {
    pub parsed: bool,
    pub accounts: Vec<GitHubAuthStatusAccount>,
}

static ENVIRONMENT_TOKEN_SOURCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:GH|GITHUB)_(?:ENTERPRISE_)?TOKEN$").unwrap());

fn non_empty_string(value: &str) -> Option<&str> {
    let trimmed = js_trim(value);
    (!trimmed.is_empty()).then_some(trimmed)
}

pub fn parse_github_auth_status(text: &str) -> GitHubAuthStatus {
    let Ok(status) = serde_json::from_str::<StatusJson>(text) else {
        return GitHubAuthStatus {
            parsed: false,
            accounts: Vec::new(),
        };
    };
    let accounts = status
        .hosts
        .into_values()
        .flatten()
        .filter_map(|account| {
            let host = non_empty_string(&account.host)?;
            let login = non_empty_string(&account.login)?;
            Some(GitHubAuthStatusAccount {
                host: host.to_lowercase(),
                account: login.to_owned(),
                authenticated: account.state == "success",
                active: account.active,
                error: account
                    .error
                    .as_deref()
                    .map(js_trim)
                    .filter(|error| !error.is_empty())
                    .map(str::to_owned),
                environment_variable: account
                    .token_source
                    .filter(|source| ENVIRONMENT_TOKEN_SOURCE.is_match(source)),
            })
        })
        .collect();
    GitHubAuthStatus {
        parsed: true,
        accounts,
    }
}

pub fn find_authenticated_github_account(
    accounts: &[GitHubAuthStatusAccount],
) -> Option<&GitHubAuthStatusAccount> {
    accounts
        .iter()
        .find(|account| account.authenticated && account.active)
        .or_else(|| accounts.iter().find(|account| account.authenticated))
}

/// The login GitHub requests for a host will use, honoring Settings: an environment token
/// first (gh's own precedence), then the pinned account, then gh's active login.
/// Returns None when the host is turned off or has no usable login.
pub fn effective_github_account<'a>(
    host: &str,
    accounts: &'a [GitHubAuthStatusAccount],
    settings: &GitHubSettings,
) -> Option<&'a GitHubAuthStatusAccount> {
    let choice = settings.host_choice(host);
    if choice.is_some_and(|choice| !choice.enabled) {
        return None;
    }
    let usable: Vec<&GitHubAuthStatusAccount> = accounts
        .iter()
        .filter(|account| account.host == host && account.authenticated)
        .collect();
    usable
        .iter()
        .find(|account| account.environment_variable.is_some())
        .copied()
        .or_else(|| {
            let pinned = choice?.account.as_ref()?;
            usable
                .iter()
                .find(|account| account.account == pinned.as_str())
                .copied()
        })
        .or_else(|| {
            usable
                .iter()
                .find(|account| account.authenticated && account.active)
                .or_else(|| usable.iter().find(|account| account.authenticated))
                .copied()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn settings(value: serde_json::Value) -> GitHubSettings {
        serde_json::from_value(value).unwrap()
    }

    fn status_json() -> String {
        json!({
            "hosts": {
                "github.com": [
                    { "state": "success", "active": false, "host": "GitHub.com", "login": "work",
                      "tokenSource": "keyring" },
                    { "state": "success", "active": true, "host": "github.com", "login": "me",
                      "tokenSource": "keyring" },
                    { "state": "error", "error": " bad credentials \n", "active": false,
                      "host": "github.com", "login": "old" },
                    { "state": "success", "active": false, "host": "github.com", "login": "  " }
                ],
                "ghe.example.com": [
                    { "state": "success", "active": true, "host": "ghe.example.com", "login": "ent",
                      "tokenSource": "GH_ENTERPRISE_TOKEN" }
                ]
            }
        })
        .to_string()
    }

    #[test]
    fn parses_accounts_lowercasing_hosts_and_dropping_blank_logins() {
        let status = parse_github_auth_status(&status_json());
        assert!(status.parsed);
        assert_eq!(
            status.accounts,
            vec![
                GitHubAuthStatusAccount {
                    host: "github.com".into(),
                    account: "work".into(),
                    authenticated: true,
                    active: false,
                    error: None,
                    environment_variable: None,
                },
                GitHubAuthStatusAccount {
                    host: "github.com".into(),
                    account: "me".into(),
                    authenticated: true,
                    active: true,
                    error: None,
                    environment_variable: None,
                },
                GitHubAuthStatusAccount {
                    host: "github.com".into(),
                    account: "old".into(),
                    authenticated: false,
                    active: false,
                    error: Some("bad credentials".into()),
                    environment_variable: None,
                },
                GitHubAuthStatusAccount {
                    host: "ghe.example.com".into(),
                    account: "ent".into(),
                    authenticated: true,
                    active: true,
                    error: None,
                    environment_variable: Some("GH_ENTERPRISE_TOKEN".into()),
                },
            ]
        );
    }

    #[test]
    fn unparseable_output_is_not_parsed() {
        for text in [
            "not json",
            "{}",
            "{\"hosts\":[]}",
            "{\"hosts\":{\"h\":[{\"state\":\"success\"}]}}",
            // `error` may be absent but not null.
            "{\"hosts\":{\"h\":[{\"state\":\"s\",\"error\":null,\"active\":true,\"host\":\"h\",\"login\":\"l\"}]}}",
        ] {
            assert_eq!(
                parse_github_auth_status(text),
                GitHubAuthStatus {
                    parsed: false,
                    accounts: vec![]
                },
                "{text}"
            );
        }
        assert!(parse_github_auth_status("{\"hosts\":{}}").parsed);
    }

    #[test]
    fn environment_variables_are_only_gh_token_kin() {
        let text = json!({ "hosts": { "h": [
            { "state": "success", "active": true, "host": "h", "login": "a", "tokenSource": "GITHUB_TOKEN" },
            { "state": "success", "active": true, "host": "h", "login": "b", "tokenSource": "OTHER_TOKEN" },
            { "state": "success", "active": true, "host": "h", "login": "c", "tokenSource": "GH_TOKEN2" }
        ]}})
        .to_string();
        let accounts = parse_github_auth_status(&text).accounts;
        assert_eq!(
            accounts
                .iter()
                .map(|a| a.environment_variable.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("GITHUB_TOKEN"), None, None]
        );
    }

    #[test]
    fn finds_the_active_authenticated_account_before_any_authenticated_one() {
        let accounts = parse_github_auth_status(&status_json()).accounts;
        assert_eq!(
            find_authenticated_github_account(&accounts)
                .unwrap()
                .account,
            "me"
        );
        assert_eq!(find_authenticated_github_account(&accounts[2..3]), None);
        assert_eq!(
            find_authenticated_github_account(&accounts[..1])
                .unwrap()
                .account,
            "work"
        );
    }

    #[test]
    fn effective_account_honors_settings() {
        let accounts = parse_github_auth_status(&status_json()).accounts;
        let account = |host: &str, settings: &GitHubSettings| {
            effective_github_account(host, &accounts, settings).map(|a| a.account.clone())
        };
        // No choice: gh's active login.
        assert_eq!(
            account("github.com", &settings(json!({}))),
            Some("me".into())
        );
        // A pinned account wins over the active one.
        assert_eq!(
            account(
                "github.com",
                &settings(json!({ "hosts": { "github.com": { "account": "work" } } }))
            ),
            Some("work".into())
        );
        // A pinned login that gh no longer holds falls back to the active one.
        assert_eq!(
            account(
                "github.com",
                &settings(json!({ "hosts": { "github.com": { "account": "old" } } }))
            ),
            Some("me".into())
        );
        // A host turned off has no account.
        assert_eq!(
            account(
                "github.com",
                &settings(json!({ "hosts": { "github.com": { "enabled": false } } }))
            ),
            None
        );
        // An environment token overrides a pinned account.
        assert_eq!(
            account(
                "ghe.example.com",
                &settings(json!({ "hosts": { "ghe.example.com": { "account": "zed" } } }))
            ),
            Some("ent".into())
        );
        assert_eq!(account("other.example", &settings(json!({}))), None);
    }
}
