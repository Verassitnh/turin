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
}
