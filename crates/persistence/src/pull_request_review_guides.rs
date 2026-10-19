//! Port of upstream/apps/server/src/persistence/PullRequestReviewGuides.ts
//! Also: Port of upstream/apps/server/src/persistence/Migrations/061_PullRequestReviewGuides.ts.
//!
//! `guide_json` stays a JSON value. Typed guide contracts are ported separately.

use rusqlite::{Connection, OptionalExtension, params};

use crate::errors::{PersistenceDecodeError, PersistenceError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadPullRequestKey {
    pub host: String,
    pub repository: String,
    pub number: i64,
}

pub fn get(
    conn: &Connection,
    key: &ThreadPullRequestKey,
) -> Result<Option<serde_json::Value>, PersistenceError> {
    let json: Option<String> = conn
        .query_row(
            "
            SELECT guide_json
            FROM pull_request_review_guides
            WHERE host = ?1 AND repository = ?2 AND number = ?3
            ",
            params![key.host, key.repository, key.number],
            |row| row.get(0),
        )
        .optional()
        .map_err(|err| PersistenceError::sql("getPullRequestReviewGuide", &err))?;
    json.map(|json| {
        serde_json::from_str(&json).map_err(|err| {
            PersistenceError::Decode(PersistenceDecodeError::new(
                "PullRequestReviewGuide",
                err.to_string(),
            ))
        })
    })
    .transpose()
}

pub fn put(
    conn: &Connection,
    key: &ThreadPullRequestKey,
    guide: &serde_json::Value,
) -> Result<(), PersistenceError> {
    let guide_json = serde_json::to_string(guide).map_err(|err| {
        PersistenceError::Decode(PersistenceDecodeError::new(
            "PullRequestReviewGuide",
            err.to_string(),
        ))
    })?;
    conn.execute(
        "
        INSERT INTO pull_request_review_guides (host, repository, number, guide_json)
        VALUES (?1, ?2, ?3, ?4)
        ON CONFLICT (host, repository, number) DO UPDATE SET guide_json = excluded.guide_json
        ",
        params![key.host, key.repository, key.number, guide_json],
    )
    .map_err(|err| PersistenceError::sql("putPullRequestReviewGuide", &err))?;
    Ok(())
}
