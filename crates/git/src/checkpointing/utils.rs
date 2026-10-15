//! Port of upstream/apps/server/src/checkpointing/Utils.ts
//!
//! `checkpoint_ref_for_scope_ordinal` is ported from
//! upstream/apps/server/src/orchestration-v2/CheckpointService.ts.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use turin_contracts::base_schemas::{CheckpointRef, CheckpointScopeId, ProjectId, ThreadId};

const CHECKPOINT_REFS_PREFIX: &str = "refs/t3/checkpoints";

/// `checkpointRefForThreadTurn`.
///
/// Node `base64url` and Effect `Base64Url.encode` are unpadded URL-safe base64.
/// [`CheckpointRef::new`] rejects a blank string; this format is never blank.
pub fn checkpoint_ref_for_thread_turn(thread_id: &ThreadId, turn_count: u64) -> CheckpointRef {
    let encoded = URL_SAFE_NO_PAD.encode(thread_id.as_str().as_bytes());
    CheckpointRef::new(format!(
        "{CHECKPOINT_REFS_PREFIX}/{encoded}/turn/{turn_count}"
    ))
    .expect("thread checkpoint ref is a non-empty trimmed string")
}

/// `checkpointRefForScopeOrdinal`.
///
/// SHA-256 of the scope id, the first 32 hex characters of that digest, then
/// base64url of those hex characters (not of the raw digest bytes).
pub fn checkpoint_ref_for_scope_ordinal(
    scope_id: &CheckpointScopeId,
    ordinal_within_scope: u64,
) -> CheckpointRef {
    let digest = Sha256::digest(scope_id.as_str().as_bytes());
    let hex = hex_encode(&digest);
    let scope_key = &hex[..32];
    let encoded = URL_SAFE_NO_PAD.encode(scope_key.as_bytes());
    CheckpointRef::new(format!(
        "{CHECKPOINT_REFS_PREFIX}/{encoded}/ordinal/{ordinal_within_scope}"
    ))
    .expect("scope checkpoint ref is a non-empty trimmed string")
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0xf) as usize] as char);
    }
    out
}

pub struct ThreadWorkspace<'a> {
    pub project_id: &'a ProjectId,
    pub worktree_path: Option<&'a str>,
}

pub struct ProjectWorkspace<'a> {
    pub id: &'a ProjectId,
    pub workspace_root: &'a str,
}

/// `resolveThreadWorkspaceCwd`. An empty worktree path is skipped, matching
/// JavaScript's falsy check on `worktreePath ?? undefined`.
pub fn resolve_thread_workspace_cwd(
    thread: &ThreadWorkspace<'_>,
    projects: &[ProjectWorkspace<'_>],
) -> Option<String> {
    if let Some(cwd) = thread.worktree_path.filter(|cwd| !cwd.is_empty()) {
        return Some(cwd.to_owned());
    }
    projects
        .iter()
        .find(|project| project.id == thread.project_id)
        .map(|project| project.workspace_root.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_turn_ref_matches_unpadded_base64url() {
        let thread_id = ThreadId::new("thread-checkpoint-store").unwrap();
        let checkpoint_ref = checkpoint_ref_for_thread_turn(&thread_id, 0);
        assert_eq!(
            checkpoint_ref.as_str(),
            "refs/t3/checkpoints/dGhyZWFkLWNoZWNrcG9pbnQtc3RvcmU/turn/0"
        );
        assert_eq!(
            checkpoint_ref_for_thread_turn(&thread_id, 7).as_str(),
            "refs/t3/checkpoints/dGhyZWFkLWNoZWNrcG9pbnQtc3RvcmU/turn/7"
        );
    }

    #[test]
    fn scope_ordinal_ref_encodes_the_hex_prefix_not_the_raw_digest() {
        let scope_id = CheckpointScopeId::new("scope:checkpoint-diff-v2:1").unwrap();
        assert_eq!(
            checkpoint_ref_for_scope_ordinal(&scope_id, 0).as_str(),
            "refs/t3/checkpoints/ZWFhMDVjMmI4NjQzYTEzNTY0YzdiNDczOWJhNGFkOWI/ordinal/0"
        );
    }

    #[test]
    fn workspace_cwd_prefers_a_worktree_then_the_project_root() {
        let project_id = ProjectId::new("project-1").unwrap();
        let other_id = ProjectId::new("project-2").unwrap();
        let projects = [
            ProjectWorkspace {
                id: &other_id,
                workspace_root: "/other",
            },
            ProjectWorkspace {
                id: &project_id,
                workspace_root: "/repo",
            },
        ];
        assert_eq!(
            resolve_thread_workspace_cwd(
                &ThreadWorkspace {
                    project_id: &project_id,
                    worktree_path: Some("/worktree"),
                },
                &projects,
            )
            .as_deref(),
            Some("/worktree")
        );
        assert_eq!(
            resolve_thread_workspace_cwd(
                &ThreadWorkspace {
                    project_id: &project_id,
                    worktree_path: Some(""),
                },
                &projects,
            )
            .as_deref(),
            Some("/repo")
        );
        assert_eq!(
            resolve_thread_workspace_cwd(
                &ThreadWorkspace {
                    project_id: &project_id,
                    worktree_path: None,
                },
                &projects,
            )
            .as_deref(),
            Some("/repo")
        );
        let missing = ProjectId::new("missing").unwrap();
        assert_eq!(
            resolve_thread_workspace_cwd(
                &ThreadWorkspace {
                    project_id: &missing,
                    worktree_path: None,
                },
                &projects,
            ),
            None
        );
    }
}
