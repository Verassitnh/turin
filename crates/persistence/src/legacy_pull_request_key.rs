//! Port of the migration-50 backfill in
//! `Migrations/050_ProjectionThreadPullRequests.ts`, including
//! `legacyThreadPullRequestKey` from `packages/shared/src/threadPullRequests.ts`
//! and the URL parser it calls.

use std::sync::OnceLock;

use regex::Regex;
use rusqlite::{Connection, params};
use turin_contracts::js_trim;
use url::Url;

use crate::errors::PersistenceError;

struct Linked {
    repository: String,
    number: i64,
    url: String,
}

struct Key {
    host: String,
    repository: String,
}

struct ChangeRequestLink {
    host: String,
    repository: String,
    number: i64,
    authority: Option<String>,
}

pub fn backfill_projection_thread_pull_requests(conn: &Connection) -> Result<(), PersistenceError> {
    let mut stmt = conn
        .prepare(
            "
            SELECT thread_id, updated_at, linked_pull_request_json
            FROM projection_threads
            WHERE linked_pull_request_json IS NOT NULL
            ",
        )
        .map_err(|err| PersistenceError::sql("ProjectionThreadPullRequests.legacy", &err))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|err| PersistenceError::sql("ProjectionThreadPullRequests.legacy", &err))?;
    let rows = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| PersistenceError::sql("ProjectionThreadPullRequests.legacy", &err))?;
    for (thread_id, updated_at, json) in rows {
        let Some(linked) = parse_legacy_linked_pull_request(&json) else {
            continue;
        };
        let key = legacy_thread_pull_request_key(&linked);
        conn.execute(
            "
            INSERT OR IGNORE INTO projection_thread_pull_requests (
              thread_id, host, repository, number, url, source, linked_at,
              snapshot_json, stack_json
            )
            VALUES (?1, ?2, ?3, ?4, ?5, 'manual', ?6, NULL, NULL)
            ",
            params![
                thread_id,
                key.host,
                key.repository,
                linked.number,
                linked.url,
                updated_at,
            ],
        )
        .map_err(|err| PersistenceError::sql("ProjectionThreadPullRequests.insert", &err))?;
    }
    Ok(())
}

fn parse_legacy_linked_pull_request(json: &str) -> Option<Linked> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let object = value.as_object()?;
    let repository = object.get("repository")?.as_str()?;
    if js_trim(repository).is_empty() {
        return None;
    }
    let number = integral_number(object.get("number")?)?;
    if number < 1 {
        return None;
    }
    let url = object.get("url")?.as_str()?;
    if js_trim(url).is_empty() {
        return None;
    }
    Some(Linked {
        repository: repository.to_owned(),
        number,
        url: url.to_owned(),
    })
}

fn legacy_thread_pull_request_key(linked: &Linked) -> Key {
    if let Some(parsed) = parse_change_request_url(&linked.url)
        && parsed.number == linked.number
    {
        let canonical = canonical_repository_key(&format!("{}/{}", parsed.host, parsed.repository));
        if parsed.authority.is_some() || canonical.starts_with("dev.azure.com/") {
            return normalize_thread_pull_request_key(&parsed);
        }
    }
    let host = http_hostname(&linked.url).unwrap_or_else(|| "unknown".to_owned());
    Key {
        host: {
            let host = js_trim(&host).to_lowercase();
            if host.is_empty() {
                "unknown".to_owned()
            } else {
                host
            }
        },
        repository: js_trim(&linked.repository).to_lowercase(),
    }
}

fn normalize_thread_pull_request_key(key: &ChangeRequestLink) -> Key {
    let identity = format!(
        "{}/{}",
        key.authority.clone().unwrap_or_else(|| key.host.clone()),
        key.repository
    );
    let canonical = canonical_repository_key(&js_trim(&identity).to_lowercase());
    let separator = canonical.find('/').unwrap_or(canonical.len());
    Key {
        host: canonical[..separator].to_owned(),
        repository: canonical[separator + 1..].to_owned(),
    }
}

fn canonical_repository_key(key: &str) -> String {
    static AZURE_SSH: OnceLock<Regex> = OnceLock::new();
    static VISUAL_STUDIO: OnceLock<Regex> = OnceLock::new();
    let azure = AZURE_SSH.get_or_init(|| {
        Regex::new(
            r"^(?:ssh\.dev\.azure\.com|vs-ssh\.visualstudio\.com)/v3/([^/]+)/([^/]+)/([^/]+)$",
        )
        .expect("azure ssh regex")
    });
    let visual = VISUAL_STUDIO.get_or_init(|| {
        Regex::new(r"^([^.]+)\.visualstudio\.com/(?:defaultcollection/)?([^/]+)/_git/([^/]+)$")
            .expect("visualstudio regex")
    });
    if let Some(caps) = azure.captures(key) {
        return format!("dev.azure.com/{}/{}/_git/{}", &caps[1], &caps[2], &caps[3]);
    }
    if let Some(caps) = visual.captures(key) {
        return format!("dev.azure.com/{}/{}/_git/{}", &caps[1], &caps[2], &caps[3]);
    }
    key.to_owned()
}

fn parse_change_request_url(target: &str) -> Option<ChangeRequestLink> {
    let url = parse_http_url(target)?;
    let host = url.host.as_str();
    if host == "git.cafe" || host == "staging.git.cafe" {
        return capture(host, &url.path, gitcafe_re(), false);
    }
    if is_host_of(host, "github.com", Some("github"))
        && let Some(link) = capture(host, &url.path, github_re(), false)
    {
        return Some(link);
    }
    if let Some(mut link) = capture(host, &url.path, forgejo_re(), false) {
        link.authority = Some(url.authority);
        return Some(link);
    }
    if let Some(link) = capture(host, &url.path, gitlab_re(), false) {
        return Some(link);
    }
    if is_host_of(host, "bitbucket.org", Some("bitbucket")) {
        return capture(host, &url.path, bitbucket_re(), false);
    }
    if is_host_of(host, "dev.azure.com", None) || host.ends_with(".visualstudio.com") {
        return capture(host, &url.path, azure_re(), false);
    }
    None
}

struct HttpUrl {
    host: String,
    authority: String,
    path: String,
}

fn parse_http_url(input: &str) -> Option<HttpUrl> {
    let url = Url::parse(input).ok()?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return None;
    }
    let host = url.host_str()?.to_lowercase();
    if host.is_empty() {
        return None;
    }
    // `URL.host` keeps a non-default port and drops the scheme default.
    let authority = match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.clone(),
    };
    Some(HttpUrl {
        host,
        authority,
        path: url.path().to_owned(),
    })
}

fn http_hostname(input: &str) -> Option<String> {
    let url = Url::parse(input).ok()?;
    let host = url.host_str()?.to_lowercase();
    if host.is_empty() { None } else { Some(host) }
}

/// `Number.isInteger`: a finite value with no fraction, including `12.0`.
fn integral_number(value: &serde_json::Value) -> Option<i64> {
    if let Some(number) = value.as_i64() {
        return Some(number);
    }
    let number = value.as_f64()?;
    if !number.is_finite() || number.fract() != 0.0 {
        return None;
    }
    if !(i64::MIN as f64..=i64::MAX as f64).contains(&number) {
        return None;
    }
    Some(number as i64)
}

fn is_host_of(hostname: &str, apex: &str, label: Option<&str>) -> bool {
    if hostname == apex
        || hostname
            .strip_suffix(apex)
            .is_some_and(|prefix| prefix.ends_with('.'))
    {
        return true;
    }
    label.is_some_and(|label| hostname.split('.').any(|part| part == label))
}

fn capture(host: &str, path: &str, re: &Regex, _unused: bool) -> Option<ChangeRequestLink> {
    let caps = re.captures(path)?;
    let repository = caps.get(1)?.as_str();
    let number: i64 = caps.get(2)?.as_str().parse().ok()?;
    if number <= 0 {
        return None;
    }
    Some(ChangeRequestLink {
        host: host.to_owned(),
        repository: repository.to_lowercase(),
        number,
        authority: None,
    })
}

fn re(source: &str) -> Regex {
    Regex::new(source).expect("pull request url regex")
}

fn gitcafe_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| re(r"^/([^/]+/[^/]+)/pulls/(\d+)(?:/|$)"))
}
fn github_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| re(r"^/([^/]+/[^/]+)/pull/(\d+)(?:/|$)"))
}
fn forgejo_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| re(r"^/([^/]+(?:/[^/]+)+)/pulls/(\d+)(?:/|$)"))
}
fn gitlab_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| re(r"^/([^/]+(?:/[^/]+)+)/-/merge_requests/(\d+)(?:/|$)"))
}
fn bitbucket_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| re(r"^/([^/]+/[^/]+)/pull-requests/(\d+)(?:/|$)"))
}
fn azure_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| re(r"^/((?:[^/]+/)*_git/[^/]+)/pullrequest/(\d+)(?:/|$)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_and_azure_keys_match_upstream() {
        let github = legacy_thread_pull_request_key(&Linked {
            repository: "Org/Repo".into(),
            number: 12,
            url: "https://github.com/Org/Repo/pull/12".into(),
        });
        assert_eq!(github.host, "github.com");
        assert_eq!(github.repository, "org/repo");

        let azure = legacy_thread_pull_request_key(&Linked {
            repository: "web".into(),
            number: 7,
            url: "https://dev.azure.com/org-a/project/_git/web/pullrequest/7".into(),
        });
        assert_eq!(azure.host, "dev.azure.com");
        assert_eq!(azure.repository, "org-a/project/_git/web");
    }

    #[test]
    fn query_uppercase_scheme_and_default_port_match_whatwg() {
        let queried = legacy_thread_pull_request_key(&Linked {
            repository: "web".into(),
            number: 7,
            url: "https://dev.azure.com/org-a/project/_git/web/pullrequest/7?_a=overview".into(),
        });
        assert_eq!(queried.host, "dev.azure.com");
        assert_eq!(queried.repository, "org-a/project/_git/web");

        let upper = legacy_thread_pull_request_key(&Linked {
            repository: "Org/Repo".into(),
            number: 12,
            url: "HTTPS://github.com/Org/Repo/pull/12".into(),
        });
        assert_eq!(upper.host, "github.com");
        assert_eq!(upper.repository, "org/repo");

        let port = legacy_thread_pull_request_key(&Linked {
            repository: "org/repo".into(),
            number: 5,
            url: "https://codeberg.org:443/org/repo/pulls/5".into(),
        });
        assert_eq!(port.host, "codeberg.org");
        assert_eq!(port.repository, "org/repo");
    }

    #[test]
    fn integral_floats_are_accepted_and_blank_js_trim_is_skipped() {
        let linked = parse_legacy_linked_pull_request(
            r#"{"repository":"acme/widgets","number":12.0,"url":"https://github.com/acme/widgets/pull/12"}"#,
        )
        .unwrap();
        assert_eq!(linked.number, 12);
        assert!(parse_legacy_linked_pull_request(
            "{\"repository\":\"\u{feff}\",\"number\":1,\"url\":\"https://github.com/a/b/pull/1\"}"
        )
        .is_none());
        assert!(
            parse_legacy_linked_pull_request(
                r#"{"repository":"acme/widgets","number":12.5,"url":"https://github.com/acme/widgets/pull/12"}"#
            )
            .is_none()
        );
    }
}
