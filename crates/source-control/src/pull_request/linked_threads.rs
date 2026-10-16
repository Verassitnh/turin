//! Port of upstream/apps/server/src/pullRequest/linkedThreads.ts

use serde_json::json;
use turin_contracts::base_schemas::{ProjectId, ThreadId, TrimmedNonEmptyString};
use turin_contracts::pull_request::{
    PullRequestLinkedThreadsEntry, PullRequestLinkedThreadsResult, PullRequestOperationError,
};
use turin_contracts::thread_pull_request::ThreadPullRequestKey;
use turin_persistence::{Db, PersistenceError};

use crate::shared::thread_pull_requests::{
    LinkedPullRequestRef, ThreadPullRequestKeySource, legacy_thread_pull_request_key,
    normalize_thread_pull_request_key, thread_pull_request_keys_equal,
};

struct Row {
    id: String,
    project_id: String,
    title: String,
    archived_at: Option<String>,
    host: Option<String>,
    repository: Option<String>,
    number: i64,
    url: Option<String>,
}

/// `listLinkedPullRequestThreads`: the live threads (archived included) that link exactly this
/// pull request, newest first. Reads both the `pullRequests` links and, for threads that predate
/// them, the single `linkedPullRequest`.
pub async fn list_linked_pull_request_threads(
    db: &Db,
    input: &ThreadPullRequestKey,
) -> Result<PullRequestLinkedThreadsResult, PullRequestOperationError> {
    list_linked(db, input)
        .await
        .map_err(|cause| PullRequestOperationError::Operation {
            operation: "linkedThreads".to_owned(),
            detail: TrimmedNonEmptyString::new("Could not load linked threads.")
                .expect("non-blank literal"),
            reason: None,
            cause: Some(json!(cause)),
        })
}

async fn list_linked(
    db: &Db,
    input: &ThreadPullRequestKey,
) -> Result<PullRequestLinkedThreadsResult, String> {
    let key = normalize_thread_pull_request_key(input);
    let number = key.number.get() as i64;
    let rows = db
        .read(move |conn| {
            let sql_error = |err: rusqlite::Error| PersistenceError::sql("listLinkedThreads", &err);
            let mut statement = conn
                .prepare(
                    r#"
      SELECT t.thread_id AS id, t.project_id AS "projectId", t.title,
        t.archived_at AS "archivedAt", json_extract(link.value, '$.host') AS host,
        json_extract(link.value, '$.repository') AS repository,
        json_extract(link.value, '$.number') AS number,
        json_extract(link.value, '$.url') AS url,
        t.updated_at AS "updatedAt"
      FROM orchestration_v2_projection_threads AS t
      JOIN json_each(t.payload_json, '$.pullRequests') AS link
      WHERE json_extract(link.value, '$.number') = ?1
        AND json_extract(link.value, '$.source') != 'stack-dismissed'
        AND t.deleted_at IS NULL
      UNION ALL
      SELECT t.thread_id AS id, t.project_id AS "projectId", t.title,
        t.archived_at AS "archivedAt", NULL AS host,
        json_extract(t.payload_json, '$.linkedPullRequest.repository') AS repository,
        json_extract(t.payload_json, '$.linkedPullRequest.number') AS number,
        json_extract(t.payload_json, '$.linkedPullRequest.url') AS url,
        t.updated_at AS "updatedAt"
      FROM orchestration_v2_projection_threads AS t
      WHERE json_type(t.payload_json, '$.pullRequests') IS NULL
        AND json_type(t.payload_json, '$.linkedPullRequest') = 'object'
        AND json_extract(t.payload_json, '$.linkedPullRequest.number') = ?1
        AND t.deleted_at IS NULL
      ORDER BY "updatedAt" DESC, id ASC
    "#,
                )
                .map_err(sql_error)?;
            let rows = statement
                .query_map([number], |row| {
                    Ok(Row {
                        id: row.get(0)?,
                        project_id: row.get(1)?,
                        title: row.get(2)?,
                        archived_at: row.get(3)?,
                        host: row.get(4)?,
                        repository: row.get(5)?,
                        number: row.get(6)?,
                        url: row.get(7)?,
                    })
                })
                .map_err(sql_error)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(sql_error)
        })
        .await
        .map_err(|error| error.to_string())?;

    let mut threads = Vec::new();
    for row in rows {
        // A link with no repository names nothing a key could match.
        let Some(repository) = row.repository.as_deref() else {
            continue;
        };
        let Ok(row_number) = u64::try_from(row.number) else {
            continue;
        };
        let url = row.url.as_deref().unwrap_or_default();
        let row_key = match row.host.as_deref() {
            None => legacy_thread_pull_request_key(
                LinkedPullRequestRef {
                    repository,
                    number: row_number,
                    url,
                },
                None,
            ),
            Some(host) => normalize_thread_pull_request_key(ThreadPullRequestKeySource {
                host,
                repository,
                number: row_number,
                authority: None,
                url: Some(url),
            }),
        };
        if thread_pull_request_keys_equal(&row_key, &key) {
            threads.push(PullRequestLinkedThreadsEntry {
                id: ThreadId::new(&row.id).map_err(|error| error.to_string())?,
                project_id: ProjectId::new(&row.project_id).map_err(|error| error.to_string())?,
                title: row.title,
                archived_at: row.archived_at,
            });
        }
    }
    Ok(PullRequestLinkedThreadsResult { threads })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use rusqlite::params;
    use turin_contracts::base_schemas::PositiveInt;

    fn key(host: &str, repository: &str, number: u64) -> ThreadPullRequestKey {
        ThreadPullRequestKey {
            host: TrimmedNonEmptyString::new(host).unwrap(),
            repository: TrimmedNonEmptyString::new(repository).unwrap(),
            number: PositiveInt::new(number).unwrap(),
        }
    }

    struct Fixture {
        id: &'static str,
        host: &'static str,
        repository: &'static str,
        number: u64,
        source: &'static str,
    }

    const fn fixture(
        id: &'static str,
        host: &'static str,
        repository: &'static str,
        number: u64,
        source: &'static str,
    ) -> Fixture {
        Fixture {
            id,
            host,
            repository,
            number,
            source,
        }
    }

    async fn ids(db: &Db, input: ThreadPullRequestKey) -> Vec<String> {
        list_linked_pull_request_threads(db, &input)
            .await
            .unwrap()
            .threads
            .into_iter()
            .map(|thread| thread.id.to_string())
            .collect()
    }

    // Upstream's Forgejo fixtures (HTTP ports recovered from stored URLs) are dropped with
    // Forgejo. The Azure ones stay: they exercise `canonical_repository_key`, which is kept.
    #[tokio::test]
    async fn finds_active_and_archived_threads_for_exactly_one_pull_request_excluding_deleted_and_dismissed_links()
     {
        let db = Db::open_in_memory().await.unwrap();
        let created_at = "2026-09-01T00:00:00.000Z";
        let archived_at = "2026-09-03T00:00:00.000Z";
        let fixtures = [
            fixture(
                "azure",
                "dev.azure.com",
                "org/project/_git/web",
                7,
                "manual",
            ),
            fixture(
                "other-org",
                "dev.azure.com",
                "other/project/_git/web",
                7,
                "manual",
            ),
            fixture("active", "github.com", "acme/web", 7, "manual"),
            fixture("archived", "github.com", "acme/web", 7, "created"),
            fixture("deleted", "github.com", "acme/web", 7, "manual"),
            fixture("dismissed", "github.com", "acme/web", 7, "stack-dismissed"),
            fixture("other-host", "github.example.com", "acme/web", 7, "manual"),
            fixture("other-repository", "github.com", "acme/api", 7, "manual"),
            fixture("other-number", "github.com", "acme/web", 8, "manual"),
        ];
        db.write(move |tx| {
            let sql = |err: rusqlite::Error| PersistenceError::sql("test", &err);
            for fixture in &fixtures {
                let payload = serde_json::json!({
                    "linkedPullRequest": null,
                    "pullRequests": [{
                        "id": fixture.id,
                        "host": fixture.host,
                        "repository": fixture.repository,
                        "number": fixture.number,
                        "source": fixture.source,
                        "linkedAt": created_at,
                        "snapshot": null,
                        "stack": null,
                    }],
                })
                .to_string();
                let archived = (fixture.id == "archived").then_some(archived_at);
                let deleted = (fixture.id == "deleted").then_some(archived_at);
                tx.execute(
                    "
                    INSERT INTO orchestration_v2_projection_threads (
                      thread_id, project_id, title, default_provider, runtime_mode, interaction_mode,
                      created_at, updated_at, archived_at, deleted_at, payload_json
                    ) VALUES (?1, 'project-1', ?1, 'codex', 'full-access', 'default', ?2, ?3, ?4, ?5, ?6)
                    ",
                    params![
                        fixture.id,
                        created_at,
                        archived.unwrap_or(created_at),
                        archived,
                        deleted,
                        payload
                    ],
                )
                .map_err(sql)?;
            }
            tx.execute(
                r#"
                INSERT INTO orchestration_v2_projection_threads (
                  thread_id, project_id, title, default_provider, runtime_mode, interaction_mode,
                  created_at, updated_at, payload_json
                ) VALUES (
                  'legacy-single', 'project-1', 'legacy-single', 'codex', 'full-access', 'default',
                  ?1, ?1,
                  '{"linkedPullRequest":{"projectId":"project-1","repository":"acme/web","number":7,"url":"https://github.com/acme/web/pull/7"}}'
                )
                "#,
                params![created_at],
            )
            .map_err(sql)?;
            tx.execute(
                r#"
                INSERT INTO orchestration_v2_projection_threads (
                  thread_id, project_id, title, default_provider, runtime_mode, interaction_mode,
                  created_at, updated_at, payload_json
                ) VALUES (
                  'explicitly-unlinked', 'project-1', 'explicitly-unlinked', 'codex', 'full-access', 'default',
                  ?1, ?1,
                  '{"linkedPullRequest":{"projectId":"project-1","repository":"acme/web","number":7,"url":"https://github.com/acme/web/pull/7"},"pullRequests":[]}'
                )
                "#,
                params![created_at],
            )
            .map_err(sql)?;
            tx.execute(
                "INSERT INTO projection_projects (
                   project_id, title, workspace_root, scripts_json, created_at, updated_at
                 ) VALUES ('project-1', 'Project', '/tmp/project', '[]', ?1, ?1)",
                params![created_at],
            )
            .map_err(sql)?;
            tx.execute(
                r#"INSERT INTO projection_threads (
                   thread_id, project_id, title, model_selection_json, created_at, updated_at
                 ) VALUES ('v1-only', 'project-1', 'v1-only',
                   '{"instanceId":"codex","model":"gpt-5.4"}', ?1, ?1)"#,
                params![created_at],
            )
            .map_err(sql)?;
            tx.execute(
                "INSERT INTO projection_thread_pull_requests (
                   thread_id, host, repository, number, url, source, linked_at
                 ) VALUES ('v1-only', 'github.com', 'acme/web', 7,
                   'https://github.com/acme/web/pull/7', 'manual', ?1)",
                params![created_at],
            )
            .map_err(sql)?;
            Ok(())
        })
        .await
        .unwrap();

        assert_eq!(
            ids(&db, key("org.visualstudio.com", "project/_git/web", 7)).await,
            vec!["azure"]
        );
        let result = list_linked_pull_request_threads(&db, &key("GitHub.Com", "ACME/WEB", 7))
            .await
            .unwrap();
        let entry = |id: &str, archived_at: Option<&str>| PullRequestLinkedThreadsEntry {
            id: ThreadId::new(id).unwrap(),
            project_id: ProjectId::new("project-1").unwrap(),
            title: id.to_owned(),
            archived_at: archived_at.map(str::to_owned),
        };
        assert_eq!(
            result,
            PullRequestLinkedThreadsResult {
                threads: vec![
                    entry("archived", Some(archived_at)),
                    entry("active", None),
                    entry("legacy-single", None),
                ],
            }
        );
        assert_eq!(
            list_linked_pull_request_threads(&db, &key("github.com", "acme/web", 99))
                .await
                .unwrap(),
            PullRequestLinkedThreadsResult { threads: vec![] }
        );
    }
}
