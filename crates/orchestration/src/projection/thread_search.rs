//! Port of upstream/apps/server/src/orchestration-v2/ThreadSearch.ts

use rusqlite::params;
use turin_contracts::thread_search::{
    OrchestrationSearchThreadsInput, OrchestrationSearchThreadsResult,
    OrchestrationThreadSearchMatch, OrchestrationThreadSearchSource,
};
use turin_contracts::{ProjectId, ThreadId};
use turin_persistence::{Db, PersistenceError};

pub fn escape_like_pattern(value: &str) -> String {
    value
        .replace('!', "!!")
        .replace('%', "!%")
        .replace('_', "!_")
}

/// Upstream's 240-character window, with the match 72 characters from its left edge.
pub fn build_search_snippet(text: &str, query: &str) -> String {
    let normalized: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = normalized.chars().collect();
    if chars.len() <= 240 {
        return normalized;
    }
    let needle = query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    let folded = normalized.to_ascii_lowercase();
    let match_index = folded
        .find(&needle)
        .map(|byte| normalized[..byte].chars().count())
        .unwrap_or(0);
    let start = match_index.saturating_sub(72).min(chars.len() - 236);
    let end = (start + 236).min(chars.len());
    format!(
        "{}{}{}",
        if start > 0 { "…" } else { "" },
        chars[start..end].iter().collect::<String>(),
        if end < chars.len() { "…" } else { "" }
    )
}

pub async fn search(
    db: &Db,
    input: OrchestrationSearchThreadsInput,
) -> Result<OrchestrationSearchThreadsResult, PersistenceError> {
    let query = input.query.as_str().to_owned();
    let pattern = format!("%{}%", escape_like_pattern(&query));
    let limit = input.limit.unwrap_or(50);
    let matches = db.read(move |conn| {
        let mut statement = conn.prepare(r#"
          WITH candidate AS (
            SELECT threads.thread_id, threads.project_id, messages.role,
              json_extract(messages.payload_json, '$.text') AS match_text,
              messages.created_at AS message_created_at, messages.message_id,
              threads.updated_at AS thread_updated_at
            FROM orchestration_v2_projection_messages AS messages
            INNER JOIN orchestration_v2_projection_threads AS threads ON threads.thread_id = messages.thread_id
            INNER JOIN projection_projects AS projects ON projects.project_id = threads.project_id
            WHERE threads.deleted_at IS NULL AND threads.archived_at IS NULL
              AND projects.deleted_at IS NULL AND messages.streaming = 0
              AND messages.role IN ('user', 'assistant')
              AND json_extract(messages.payload_json, '$.text') LIKE ?1 ESCAPE '!'
          ), ranked AS (
            SELECT thread_id, project_id, role AS source, match_text, message_created_at,
              CASE role WHEN 'user' THEN 0 ELSE 1 END AS match_rank,
              thread_updated_at,
              ROW_NUMBER() OVER (PARTITION BY thread_id ORDER BY
                CASE role WHEN 'user' THEN 0 ELSE 1 END ASC,
                message_created_at DESC, message_id ASC) AS thread_match_rank
            FROM candidate
          )
          SELECT thread_id, project_id, source, match_text, message_created_at FROM ranked
          WHERE thread_match_rank = 1
          ORDER BY match_rank ASC, thread_updated_at DESC, thread_id ASC LIMIT ?2
        "#).map_err(|e| PersistenceError::sql("ThreadSearch.search", &e))?;
        let rows = statement.query_map(params![pattern, limit], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?, row.get::<_, Option<String>>(4)?))
        }).map_err(|e| PersistenceError::sql("ThreadSearch.search", &e))?;
        let mut matches = Vec::new();
        for row in rows {
            let (thread_id, project_id, source, text, message_created_at) = row.map_err(|e| PersistenceError::sql("ThreadSearch.search", &e))?;
            matches.push(OrchestrationThreadSearchMatch {
                thread_id: ThreadId::new(thread_id).map_err(|e| PersistenceError::Decode(turin_persistence::PersistenceDecodeError::new("ThreadSearch.search", e.to_string())))?,
                project_id: ProjectId::new(project_id).map_err(|e| PersistenceError::Decode(turin_persistence::PersistenceDecodeError::new("ThreadSearch.search", e.to_string())))?,
                source: if source == "user" { OrchestrationThreadSearchSource::User } else { OrchestrationThreadSearchSource::Assistant },
                snippet: build_search_snippet(&text, &query), message_created_at,
            });
        }
        Ok(matches)
    }).await?;
    Ok(OrchestrationSearchThreadsResult { matches })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn escapes_like_metacharacters() {
        assert_eq!(escape_like_pattern("!%_"), "!!!%!_");
    }
    #[test]
    fn snippet_centers_long_match_and_caps_length() {
        let text = format!("{}needle{}", "a".repeat(130), "b".repeat(200));
        let snippet = build_search_snippet(&text, "needle");
        assert!(snippet.contains("needle"));
        assert!(snippet.chars().count() <= 240);
    }

    use crate::projection::test_support::*;
    use serde_json::json;
    use turin_contracts::thread_search::OrchestrationSearchThreadsInput;

    fn input(query: &str, limit: Option<u32>) -> OrchestrationSearchThreadsInput {
        serde_json::from_value(match limit {
            Some(limit) => json!({"query": query, "limit": limit}),
            None => json!({"query": query}),
        })
        .unwrap()
    }

    fn message_event(
        thread: &str,
        id: &str,
        role: &str,
        text: &str,
        minute: u32,
        streaming: bool,
    ) -> turin_contracts::orchestration_v2::OrchestrationV2DomainEvent {
        let stamp = format!("2026-09-27T00:{minute:02}:00.000Z");
        let mut value = message(thread, id, None, role, text, &stamp);
        value["streaming"] = json!(streaming);
        event("message.updated", thread, &stamp, value)
    }

    async fn create_project(db: &Db, id: &str, deleted: bool) {
        let id = id.to_owned();
        db.write(move |tx| {
            let created = turin_persistence::ProjectEvent {
                sequence: 0,
                event_id: turin_contracts::EventId::new(format!("created:{id}")).unwrap(),
                event_type: "project.created".into(),
                aggregate_kind: "project".into(),
                aggregate_id: id.clone(),
                occurred_at: "2026-09-27T00:00:00.000Z".into(),
                command_id: None,
                causation_event_id: None,
                correlation_id: None,
                payload: json!({"projectId": id, "title": id, "workspaceRoot": format!("/work/{id}"),
                    "defaultModelSelection": null, "scripts": [],
                    "createdAt": "2026-09-27T00:00:00.000Z", "updatedAt": "2026-09-27T00:00:00.000Z"}),
                metadata: json!({}),
            };
            crate::projection::project_store::ProjectStore::apply_in_tx(tx, &created)?;
            if deleted {
                let removed = turin_persistence::ProjectEvent {
                    event_type: "project.deleted".into(),
                    payload: json!({"projectId": id, "deletedAt": "2026-09-27T00:02:00.000Z"}),
                    ..created
                };
                crate::projection::project_store::ProjectStore::apply_in_tx(tx, &removed)?;
            }
            Ok(())
        })
        .await
        .unwrap();
    }

    fn thread_in(
        id: &str,
        project: &str,
        archived: bool,
        deleted: bool,
    ) -> turin_contracts::orchestration_v2::OrchestrationV2DomainEvent {
        let mut value = thread(id);
        value["projectId"] = json!(project);
        if archived {
            value["archivedAt"] = json!(T0);
        }
        if deleted {
            value["deletedAt"] = json!(T0);
        }
        event("thread.created", id, T0, value)
    }

    #[tokio::test]
    async fn returns_one_finished_user_or_assistant_match_per_active_thread() {
        let store = memory_store().await;
        create_project(store.db(), "project:search", false).await;
        create_project(store.db(), "project:search-deleted", true).await;
        apply_all(
            &store,
            vec![
                thread_in("thread:both", "project:search", false, false),
                message_event(
                    "thread:both",
                    "both-assistant",
                    "assistant",
                    "needle from the answer",
                    3,
                    false,
                ),
                message_event(
                    "thread:both",
                    "both-user-old",
                    "user",
                    "older needle question",
                    1,
                    false,
                ),
                message_event(
                    "thread:both",
                    "both-user-new",
                    "user",
                    "newer needle question",
                    2,
                    false,
                ),
                thread_in("thread:assistant", "project:search", false, false),
                message_event(
                    "thread:assistant",
                    "assistant-only",
                    "assistant",
                    "needle in an answer",
                    1,
                    false,
                ),
                message_event(
                    "thread:assistant",
                    "assistant-streaming",
                    "user",
                    "needle still typing",
                    1,
                    true,
                ),
                message_event(
                    "thread:assistant",
                    "assistant-system",
                    "system",
                    "needle system prompt",
                    1,
                    false,
                ),
                thread_in("thread:archived", "project:search", true, false),
                message_event(
                    "thread:archived",
                    "archived",
                    "user",
                    "needle archived",
                    1,
                    false,
                ),
                thread_in("thread:deleted", "project:search", false, true),
                message_event(
                    "thread:deleted",
                    "deleted",
                    "user",
                    "needle deleted",
                    1,
                    false,
                ),
                thread_in("thread:orphaned", "project:search-deleted", false, false),
                message_event(
                    "thread:orphaned",
                    "orphaned",
                    "user",
                    "needle orphaned",
                    1,
                    false,
                ),
            ],
        )
        .await;
        let result = search(store.db(), input("NEEDLE", Some(20))).await.unwrap();
        let got: Vec<_> = result
            .matches
            .iter()
            .map(|found| {
                (
                    found.thread_id.to_string(),
                    found.source,
                    found.snippet.clone(),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                (
                    "thread:both".to_owned(),
                    OrchestrationThreadSearchSource::User,
                    "newer needle question".to_owned()
                ),
                (
                    "thread:assistant".to_owned(),
                    OrchestrationThreadSearchSource::Assistant,
                    "needle in an answer".to_owned()
                ),
            ]
        );
        assert_eq!(
            search(store.db(), input("needle", Some(1)))
                .await
                .unwrap()
                .matches
                .len(),
            1
        );
        // LIKE wildcards in the query match literally.
        assert!(
            search(store.db(), input("ne%le", None))
                .await
                .unwrap()
                .matches
                .is_empty()
        );
    }

    #[tokio::test]
    async fn searches_on_the_reader_while_the_writer_is_busy() {
        let (store, _dir) = file_store().await;
        create_project(store.db(), "project:search-reader", false).await;
        apply_all(
            &store,
            vec![
                thread_in("thread:reader", "project:search-reader", false, false),
                message_event(
                    "thread:reader",
                    "reader",
                    "user",
                    "needle on the reader",
                    1,
                    false,
                ),
            ],
        )
        .await;
        // A write transaction holds the writer. A search on the writer would queue
        // behind it and never finish; one on the reader does not wait.
        let (held_tx, held_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let db = store.db().clone();
        let writer = tokio::spawn(async move {
            db.write(move |_tx| {
                held_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
            .await
        });
        held_rx.await.unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            search(store.db(), input("needle", None)),
        )
        .await
        .expect("search must not wait for the writer")
        .unwrap();
        release_tx.send(()).unwrap();
        writer.await.unwrap().unwrap();
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].thread_id.as_str(), "thread:reader");
    }
}
