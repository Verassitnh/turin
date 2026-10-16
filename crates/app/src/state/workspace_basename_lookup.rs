//! Port of upstream/apps/web/src/workspaceBasenameLookup.ts
//!
//! Clicking a bare filename in chat markdown (`ChatView.tsx`) has no directory
//! to resolve against, so ChatMarkdown asks a workspace search for it. These
//! are the decisions around that search.

use std::sync::atomic::{AtomicU64, Ordering};

/// Enough hits to look past same-named neighbours (`ChatView.test.tsx`) without
/// asking for a full listing on a single click.
pub const WORKSPACE_BASENAME_LOOKUP_LIMIT: usize = 25;

/// A claim on the lookup sequence. Ask [`WorkspaceBasenameLookupClaim::is_latest`]
/// when the search settles; false means a later click superseded it.
#[derive(Debug, Clone)]
pub struct WorkspaceBasenameLookupClaim {
    sequence: &'static AtomicU64,
    claimed: u64,
}

impl WorkspaceBasenameLookupClaim {
    /// Upstream's returned `() => boolean`.
    pub fn is_latest(&self) -> bool {
        self.claimed == self.sequence.load(Ordering::SeqCst)
    }
}

/// One counter for every caller: they all open the same panel, so the newest
/// click wins regardless of which one started the lookup.
static LATEST_LOOKUP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// `claimWorkspaceBasenameLookup`.
pub fn claim_workspace_basename_lookup() -> WorkspaceBasenameLookupClaim {
    claim_on(&LATEST_LOOKUP_SEQUENCE)
}

fn claim_on(sequence: &'static AtomicU64) -> WorkspaceBasenameLookupClaim {
    let claimed = sequence.fetch_add(1, Ordering::SeqCst) + 1;
    WorkspaceBasenameLookupClaim { sequence, claimed }
}

/// `WorkspaceEntryCandidate.kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceEntryKind {
    File,
    Directory,
}

/// `WorkspaceEntryCandidate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceEntryCandidate {
    pub path: String,
    pub kind: WorkspaceEntryKind,
}

fn basename_of_path(path: &str) -> &str {
    match path.rfind(['/', '\\']) {
        Some(index) => &path[index + 1..],
        None => path,
    }
}

pub fn needs_workspace_basename_lookup(relative_path: &str) -> bool {
    let trimmed = turin_contracts::base_schemas::js_trim(relative_path);
    trimmed != "."
        && trimmed != ".."
        && !trimmed.is_empty()
        && !trimmed.contains('/')
        && !trimmed.contains('\\')
}

pub fn pick_workspace_basename_match(
    basename: &str,
    entries: &[WorkspaceEntryCandidate],
) -> Option<String> {
    let target = turin_contracts::base_schemas::js_trim(basename);
    if target.is_empty() {
        return None;
    }
    let files = || {
        entries
            .iter()
            .filter(|entry| entry.kind == WorkspaceEntryKind::File)
    };
    if let Some(exact) = files().find(|entry| basename_of_path(&entry.path) == target) {
        return Some(exact.path.clone());
    }
    // Folded matching covers casing that drifted from disk, but `FOO.ts` against
    // both `Foo.ts` and `foo.ts` has no right answer, so it resolves to nothing
    // rather than opening whichever the index ranked first.
    let folded = target.to_lowercase();
    let mut folded_matches =
        files().filter(|entry| basename_of_path(&entry.path).to_lowercase() == folded);
    match (folded_matches.next(), folded_matches.next()) {
        (Some(only), None) => Some(only.path.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str) -> WorkspaceEntryCandidate {
        WorkspaceEntryCandidate {
            path: path.to_string(),
            kind: WorkspaceEntryKind::File,
        }
    }

    fn directory(path: &str) -> WorkspaceEntryCandidate {
        WorkspaceEntryCandidate {
            path: path.to_string(),
            kind: WorkspaceEntryKind::Directory,
        }
    }

    #[test]
    fn flags_bare_filenames() {
        assert!(needs_workspace_basename_lookup("ChatView.tsx"));
        assert!(needs_workspace_basename_lookup("Makefile"));
    }

    #[test]
    fn leaves_anything_with_a_directory_alone() {
        assert!(!needs_workspace_basename_lookup(
            "apps/web/src/components/ChatView.tsx"
        ));
        assert!(!needs_workspace_basename_lookup("apps\\web\\ChatView.tsx"));
        assert!(!needs_workspace_basename_lookup("."));
        assert!(!needs_workspace_basename_lookup(".."));
        assert!(!needs_workspace_basename_lookup("   "));
    }

    fn chat_view_entries() -> Vec<WorkspaceEntryCandidate> {
        vec![
            file("apps/web/src/components/ChatView.test.tsx"),
            file("apps/web/src/components/ChatView.tsx"),
        ]
    }

    #[test]
    fn takes_the_first_exact_filename_match_not_the_closest_fuzzy_one() {
        assert_eq!(
            pick_workspace_basename_match("ChatView.tsx", &chat_view_entries()).as_deref(),
            Some("apps/web/src/components/ChatView.tsx")
        );
    }

    #[test]
    fn ignores_directories() {
        assert_eq!(
            pick_workspace_basename_match(
                "components",
                &[
                    directory("apps/web/src/components"),
                    file("apps/web/src/components/components"),
                ]
            )
            .as_deref(),
            Some("apps/web/src/components/components")
        );
    }

    #[test]
    fn prefers_the_exactly_cased_file_over_a_case_only_twin() {
        assert_eq!(
            pick_workspace_basename_match("foo.ts", &[file("src/Foo.ts"), file("src/foo.ts")])
                .as_deref(),
            Some("src/foo.ts")
        );
    }

    #[test]
    fn falls_back_to_case_insensitive_when_only_the_casing_differs() {
        assert_eq!(
            pick_workspace_basename_match("chatview.tsx", &chat_view_entries()).as_deref(),
            Some("apps/web/src/components/ChatView.tsx")
        );
    }

    #[test]
    fn returns_none_when_the_case_insensitive_fallback_is_ambiguous() {
        assert_eq!(
            pick_workspace_basename_match("FOO.ts", &[file("src/Foo.ts"), file("src/foo.ts")]),
            None
        );
    }

    #[test]
    fn returns_none_when_nothing_matches_the_name() {
        assert_eq!(pick_workspace_basename_match("ChatView.tsx", &[]), None);
        assert_eq!(
            pick_workspace_basename_match(
                "ChatView.tsx",
                &[file("apps/web/src/components/ChatHeader.tsx")]
            ),
            None
        );
    }

    #[test]
    fn blank_basename_matches_nothing() {
        assert_eq!(
            pick_workspace_basename_match("  ", &chat_view_entries()),
            None
        );
    }

    #[test]
    fn keeps_only_the_newest_claim_whatever_order_the_lookups_settle_in() {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let first = claim_on(&SEQUENCE);
        let second = claim_on(&SEQUENCE);

        // The older lookup answering last must not reopen the panel behind the
        // newer one.
        assert!(second.is_latest());
        assert!(!first.is_latest());
    }

    #[test]
    fn stays_valid_while_it_is_the_only_claim() {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let only = claim_on(&SEQUENCE);
        assert!(only.is_latest());
    }

    #[test]
    fn the_shared_counter_hands_out_claims() {
        // Other tests may claim concurrently, so only the claim itself is checked.
        let _ = claim_workspace_basename_lookup();
        assert_eq!(WORKSPACE_BASENAME_LOOKUP_LIMIT, 25);
    }
}
