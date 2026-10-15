//! Port of the migration-50 backfill in
//! `Migrations/050_ProjectionThreadPullRequests.ts`, including
//! `legacyThreadPullRequestKey` from `packages/shared/src/threadPullRequests.ts`
//! and the URL parser it calls.

use std::sync::OnceLock;

use regex::Regex;
use rusqlite::{Connection, params};

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
    if repository.trim().is_empty() {
        return None;
    }
    let number = object.get("number")?.as_i64()?;
    if number < 1 {
        return None;
    }
    let url = object.get("url")?.as_str()?;
    if url.trim().is_empty() {
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
            let host = host.trim().to_ascii_lowercase();
            if host.is_empty() {
                "unknown".to_owned()
            } else {
                host
            }
        },
        repository: linked.repository.trim().to_ascii_lowercase(),
    }
}

fn normalize_thread_pull_request_key(key: &ChangeRequestLink) -> Key {
    let identity = format!(
        "{}/{}",
        key.authority.clone().unwrap_or_else(|| key.host.clone()),
        key.repository
    );
    let canonical = canonical_repository_key(&identity.trim().to_ascii_lowercase());
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
    let rest = input
        .strip_prefix("https://")
        .or_else(|| input.strip_prefix("http://"))?;
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    let authority = authority
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(authority);
    if authority.is_empty() {
        return None;
    }
    let host = authority.split(':').next()?.to_ascii_lowercase();
    if host.is_empty() {
        return None;
    }
    Some(HttpUrl {
        host,
        authority: authority.to_ascii_lowercase(),
        path: path.to_owned(),
    })
}

fn http_hostname(input: &str) -> Option<String> {
    parse_http_url(input).map(|url| url.host)
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
        repository: repository.to_ascii_lowercase(),
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
}
