//! Port of upstream/apps/web/src/workspaceBasenameLookup.ts
//!
//! Resolving a bare filename in chat (`ChatView.tsx`) to a workspace path: when to look it up,
//! which search hit to take, and the claim that lets only the newest click open the panel.

use std::sync::atomic::{AtomicU64, Ordering};

/// Enough hits to look past same-named neighbours (`ChatView.test.tsx`) without asking for a
/// full listing on a single click.
pub const WORKSPACE_BASENAME_LOOKUP_LIMIT: usize = 25;

/// A counter of lookups. One is shared by every caller (see [`claim_workspace_basename_lookup`]):
/// they all open the same panel, so the newest click wins regardless of which one started the
/// lookup.
#[derive(Debug, Default)]
pub struct LookupSequence(AtomicU64);

impl LookupSequence {
    pub const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    /// Call the returned predicate when the search settles; `false` means a later click
    /// superseded it.
    pub fn claim(&self) -> impl Fn() -> bool + use<'_> {
        let claimed = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        move || claimed == self.0.load(Ordering::SeqCst)
    }
}

static LATEST_LOOKUP: LookupSequence = LookupSequence::new();

/// `claimWorkspaceBasenameLookup`.
pub fn claim_workspace_basename_lookup() -> impl Fn() -> bool {
    LATEST_LOOKUP.claim()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceEntryKind {
    File,
    Directory,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceEntryCandidate {
    pub path: String,
    pub kind: WorkspaceEntryKind,
}

fn basename_of_path(path: &str) -> &str {
    path.rfind(['/', '\\'])
        .map_or(path, |separator| &path[separator + 1..])
}

pub fn needs_workspace_basename_lookup(relative_path: &str) -> bool {
    let trimmed = relative_path.trim();
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
    let target = basename.trim();
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
    // Folded matching covers casing that drifted from disk, but `FOO.ts` against both `Foo.ts`
    // and `foo.ts` has no right answer, so it resolves to nothing rather than opening whichever
    // the index ranked first.
    let folded = target.to_lowercase();
    let mut matches =
        files().filter(|entry| basename_of_path(&entry.path).to_lowercase() == folded);
    match (matches.next(), matches.next()) {
        (Some(only), None) => Some(only.path.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str) -> WorkspaceEntryCandidate {
        WorkspaceEntryCandidate {
            path: path.into(),
            kind: WorkspaceEntryKind::File,
        }
    }

    fn directory(path: &str) -> WorkspaceEntryCandidate {
        WorkspaceEntryCandidate {
            path: path.into(),
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

    // The shared counter is process-wide, so each case claims from its own sequence; the
    // behaviour under test is the same.
    #[test]
    fn keeps_only_the_newest_claim_whatever_order_the_lookups_settle_in() {
        let sequence = LookupSequence::new();
        let first = sequence.claim();
        let second = sequence.claim();
        // The older lookup answering last must not reopen the panel behind the newer one.
        assert!(second());
        assert!(!first());
    }

    #[test]
    fn stays_valid_while_it_is_the_only_claim() {
        let sequence = LookupSequence::new();
        let only = sequence.claim();
        assert!(only());
    }

    #[test]
    fn the_shared_claim_supersedes_an_older_one() {
        let older = claim_workspace_basename_lookup();
        let newer = claim_workspace_basename_lookup();
        // Other tests may claim from the shared counter too, so only the older claim's
        // supersession is certain.
        assert!(!older());
        let _ = newer;
    }
}
