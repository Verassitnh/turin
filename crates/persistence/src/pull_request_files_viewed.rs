//! Port of upstream/apps/server/src/persistence/PullRequestFilesViewed.ts

use rusqlite::{Connection, params};

use crate::errors::PersistenceError;

/// How many marks one read of this store carries. Every one of them is a path
/// held for as long as the caller holds the read, so an unbounded read of a
/// change request with thousands of marks is paid for again per scope.
/// Matched to what the GitHub reader walks in one go.
pub const MAX_FILES_VIEWED_ROWS: i64 = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Host is part of the scope because the same `group/project` exists on
/// gitlab.com and on a self-managed instance, and the viewer because signing
/// in as somebody else must not inherit their ticks.
pub struct PullRequestFilesViewedScope {
    pub provider: String,
    pub host: String,
    pub repository: String,
    pub number: i64,
    pub viewer: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestFileViewedMark {
    pub path: String,
    /// `None` is SQL NULL: no baseline, not an empty revision.
    pub revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestFileViewedWrite {
    pub path: String,
    pub revision: Option<String>,
    pub viewed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestFilesViewedPage {
    pub files: Vec<PullRequestFileViewedMark>,
    pub truncated: bool,
}

/// Ordered by path and read one row past the ceiling, so the same marks come
/// back on every read rather than a window that shuffles, and having more than
/// were carried is known rather than guessed at from a full page.
pub fn list(
    conn: &Connection,
    scope: &PullRequestFilesViewedScope,
) -> Result<PullRequestFilesViewedPage, PersistenceError> {
    let mut stmt = conn
        .prepare_cached(
            "
            SELECT path, revision
            FROM pull_request_files_viewed
            WHERE provider = ?1 AND host = ?2 AND repository = ?3
              AND number = ?4 AND viewer = ?5
            ORDER BY path
            LIMIT ?6
            ",
        )
        .map_err(|err| PersistenceError::sql("listPullRequestFilesViewed", &err))?;
    let rows = stmt
        .query_map(
            params![
                scope.provider,
                scope.host,
                scope.repository,
                scope.number,
                scope.viewer,
                MAX_FILES_VIEWED_ROWS + 1
            ],
            |row| {
                Ok(PullRequestFileViewedMark {
                    path: row.get(0)?,
                    revision: row.get(1)?,
                })
            },
        )
        .map_err(|err| PersistenceError::sql("listPullRequestFilesViewed", &err))?;
    let mut files = Vec::new();
    for row in rows {
        files.push(row.map_err(|err| PersistenceError::sql("listPullRequestFilesViewed", &err))?);
    }
    let truncated = files.len() as i64 > MAX_FILES_VIEWED_ROWS;
    files.truncate(MAX_FILES_VIEWED_ROWS as usize);
    Ok(PullRequestFilesViewedPage { files, truncated })
}

/// One statement per file rather than one for the batch: a mixed batch of
/// clears and un-clears has no single statement. The caller wraps the batch
/// in one transaction so a failure part way through does not leave some files
/// cleared and the rest not.
pub fn set(
    conn: &Connection,
    scope: &PullRequestFilesViewedScope,
    files: &[PullRequestFileViewedWrite],
    viewed_at: &str,
) -> Result<(), PersistenceError> {
    for file in files {
        if file.viewed {
            conn.prepare_cached(
                "
                INSERT INTO pull_request_files_viewed (
                  provider, host, repository, number, viewer, path, revision, viewed_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                ON CONFLICT (provider, host, repository, number, viewer, path)
                DO UPDATE SET revision = excluded.revision, viewed_at = excluded.viewed_at
                ",
            )
            .and_then(|mut stmt| {
                stmt.execute(params![
                    scope.provider,
                    scope.host,
                    scope.repository,
                    scope.number,
                    scope.viewer,
                    file.path,
                    file.revision,
                    viewed_at,
                ])
            })
            .map_err(|err| PersistenceError::sql("setPullRequestFilesViewed", &err))?;
        } else {
            conn.prepare_cached(
                "
                DELETE FROM pull_request_files_viewed
                WHERE provider = ?1 AND host = ?2 AND repository = ?3
                  AND number = ?4 AND viewer = ?5 AND path = ?6
                ",
            )
            .and_then(|mut stmt| {
                stmt.execute(params![
                    scope.provider,
                    scope.host,
                    scope.repository,
                    scope.number,
                    scope.viewer,
                    file.path,
                ])
            })
            .map_err(|err| PersistenceError::sql("setPullRequestFilesViewed", &err))?;
        }
    }
    Ok(())
}
