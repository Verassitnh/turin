//! Port of upstream/apps/web/src/components/pullRequest/pullRequestFilesViewed.logic.ts
//! and the pure state transition from usePullRequestFilesViewed.ts. The React hook, timers,
//! query, and DOM rendering are skipped.

use indexmap::IndexMap;
use std::collections::{HashMap, HashSet};
use turin_contracts::pull_request::{
    PullRequestFileViewedState, PullRequestFilesViewedResult, PullRequestSetFilesViewedFile,
};

pub type FileViewedStates = IndexMap<String, PullRequestFileViewedState>;
pub type FileViewedOverlay = IndexMap<String, bool>;
/// How long presses gather before the host is told.
pub const FLUSH_DELAY_MS: u64 = 400;
pub fn to_file_viewed_states(
    result: Option<&PullRequestFilesViewedResult>,
) -> Option<FileViewedStates> {
    result.map(|result| {
        result
            .files
            .iter()
            .map(|file| (file.path.clone(), file.state))
            .collect()
    })
}
pub fn is_stale_viewed_state(state: Option<PullRequestFileViewedState>) -> bool {
    state == Some(PullRequestFileViewedState::Dismissed)
}
pub fn is_file_viewed(
    path: &str,
    states: Option<&FileViewedStates>,
    overlay: &FileViewedOverlay,
) -> bool {
    overlay.get(path).copied().unwrap_or_else(|| {
        states.and_then(|s| s.get(path)).copied() == Some(PullRequestFileViewedState::Viewed)
    })
}
pub fn count_viewed_files(
    paths: &[String],
    states: Option<&FileViewedStates>,
    overlay: &FileViewedOverlay,
) -> usize {
    paths
        .iter()
        .filter(|path| is_file_viewed(path, states, overlay))
        .count()
}
/// Retain presses until a read that could have seen the acknowledged write returns.
pub fn settle_file_viewed_overlay(
    overlay: &FileViewedOverlay,
    states: Option<&FileViewedStates>,
    pending: &HashSet<String>,
    answered: &HashSet<String>,
) -> FileViewedOverlay {
    let Some(states) = states else {
        return overlay.clone();
    };
    let mut next = overlay.clone();
    for (path, pressed) in overlay {
        if !pending.contains(path)
            && (answered.contains(path)
                || (states.get(path) == Some(&PullRequestFileViewedState::Viewed)) == *pressed)
        {
            next.remove(path);
        }
    }
    next
}
/// A failed request only takes back presses it still owns.
pub fn revert_file_viewed_overlay(
    overlay: &FileViewedOverlay,
    batch: &[PullRequestSetFilesViewedFile],
    owned: &HashSet<String>,
) -> FileViewedOverlay {
    let mut next = overlay.clone();
    for file in batch {
        if owned.contains(&file.path) && next.get(&file.path) == Some(&file.viewed) {
            next.remove(&file.path);
        }
    }
    next
}
pub fn to_file_viewed_batch(overlay: &FileViewedOverlay) -> Vec<PullRequestSetFilesViewedFile> {
    overlay
        .iter()
        .map(|(path, viewed)| PullRequestSetFilesViewedFile {
            path: path.clone(),
            viewed: *viewed,
        })
        .collect()
}
/// A path remains stale only where no local press covers the host's dismissal.
pub fn is_file_viewed_stale(
    path: &str,
    states: Option<&FileViewedStates>,
    overlay: &FileViewedOverlay,
) -> bool {
    !overlay.contains_key(path) && is_stale_viewed_state(states.and_then(|s| s.get(path)).copied())
}
/// First changed states object after acknowledgement answers a press, even if it contradicts it.
pub fn answered_paths_after_read(
    acknowledged_from: &HashMap<String, usize>,
    current_states_identity: usize,
    pending: &HashSet<String>,
) -> HashSet<String> {
    acknowledged_from
        .iter()
        .filter(|(path, from)| !pending.contains(*path) && **from != current_states_identity)
        .map(|(path, _)| path.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    fn map(entries: &[(&str, bool)]) -> FileViewedOverlay {
        entries.iter().map(|(p, v)| ((*p).to_owned(), *v)).collect()
    }
    fn set(paths: &[&str]) -> HashSet<String> {
        paths.iter().map(|p| (*p).to_owned()).collect()
    }
    fn states() -> FileViewedStates {
        [
            ("a.ts", PullRequestFileViewedState::Viewed),
            ("b.ts", PullRequestFileViewedState::Unviewed),
            ("c.ts", PullRequestFileViewedState::Dismissed),
        ]
        .into_iter()
        .map(|(p, s)| (p.to_owned(), s))
        .collect()
    }
    #[test]
    fn follows_host_when_unpressed() {
        assert!(is_file_viewed("a.ts", Some(&states()), &map(&[])));
        assert!(!is_file_viewed("b.ts", Some(&states()), &map(&[])));
    }
    #[test]
    fn pushed_file_is_unread_and_stale() {
        assert!(!is_file_viewed("c.ts", Some(&states()), &map(&[])));
        assert!(is_stale_viewed_state(states().get("c.ts").copied()));
        assert!(!is_stale_viewed_state(states().get("a.ts").copied()));
    }
    #[test]
    fn press_precedes_host() {
        assert!(is_file_viewed(
            "b.ts",
            Some(&states()),
            &map(&[("b.ts", true)])
        ));
        assert!(!is_file_viewed(
            "a.ts",
            Some(&states()),
            &map(&[("a.ts", false)])
        ));
    }
    #[test]
    fn no_host_answer_yet() {
        assert!(!is_file_viewed("z.ts", None, &map(&[])));
        assert!(is_file_viewed("z.ts", None, &map(&[("z.ts", true)])));
    }
    #[test]
    fn counts_screen_paths_only() {
        let paths = ["a.ts", "b.ts", "c.ts"].map(str::to_owned);
        assert_eq!(count_viewed_files(&paths, Some(&states()), &map(&[])), 1);
        assert_eq!(
            count_viewed_files(&paths, Some(&states()), &map(&[("b.ts", true)])),
            2
        );
        assert_eq!(
            count_viewed_files(&["b.ts".to_owned()], Some(&states()), &map(&[])),
            0
        );
    }
    #[test]
    fn drops_caught_up_press() {
        assert!(
            settle_file_viewed_overlay(
                &map(&[("a.ts", true)]),
                Some(&states()),
                &set(&[]),
                &set(&[])
            )
            .is_empty()
        );
    }
    #[test]
    fn keeps_disagreeing_press() {
        assert_eq!(
            settle_file_viewed_overlay(
                &map(&[("b.ts", true)]),
                Some(&states()),
                &set(&[]),
                &set(&[])
            ),
            map(&[("b.ts", true)])
        );
    }
    #[test]
    fn keeps_unheard_press() {
        assert_eq!(
            settle_file_viewed_overlay(
                &map(&[("a.ts", false)]),
                Some(&states()),
                &set(&["a.ts"]),
                &set(&[])
            ),
            map(&[("a.ts", false)])
        );
    }
    #[test]
    fn settles_dismissed_against_untick() {
        assert!(
            settle_file_viewed_overlay(
                &map(&[("c.ts", false)]),
                Some(&states()),
                &set(&[]),
                &set(&[])
            )
            .is_empty()
        );
    }
    #[test]
    fn answered_read_drops_contradicted_tick() {
        let settled = settle_file_viewed_overlay(
            &map(&[("c.ts", true)]),
            Some(&states()),
            &set(&[]),
            &set(&["c.ts"]),
        );
        assert!(settled.is_empty());
        assert!(!is_file_viewed("c.ts", Some(&states()), &settled));
    }
    #[test]
    fn new_press_outlives_prior_answer() {
        assert_eq!(
            settle_file_viewed_overlay(
                &map(&[("c.ts", true)]),
                Some(&states()),
                &set(&["c.ts"]),
                &set(&["c.ts"])
            ),
            map(&[("c.ts", true)])
        );
    }
    #[test]
    fn holds_until_any_host_answer() {
        assert_eq!(
            settle_file_viewed_overlay(&map(&[("a.ts", true)]), None, &set(&[]), &set(&[])),
            map(&[("a.ts", true)])
        );
    }
    #[test]
    fn batch_carries_both_directions() {
        let batch = to_file_viewed_batch(&map(&[("a.ts", false), ("b.ts", true)]));
        assert_eq!(batch.len(), 2);
        assert!(batch.iter().any(|f| f.path == "a.ts" && !f.viewed));
        assert!(batch.iter().any(|f| f.path == "b.ts" && f.viewed));
    }
    fn batch() -> Vec<PullRequestSetFilesViewedFile> {
        vec![
            PullRequestSetFilesViewedFile {
                path: "a.ts".to_owned(),
                viewed: true,
            },
            PullRequestSetFilesViewedFile {
                path: "b.ts".to_owned(),
                viewed: false,
            },
        ]
    }
    #[test]
    fn failure_reverts_owned_press() {
        assert!(
            revert_file_viewed_overlay(
                &map(&[("a.ts", true), ("b.ts", false)]),
                &batch(),
                &set(&["a.ts", "b.ts"])
            )
            .is_empty()
        );
    }
    #[test]
    fn failure_preserves_later_press() {
        assert_eq!(
            revert_file_viewed_overlay(
                &map(&[("a.ts", false), ("b.ts", false)]),
                &batch(),
                &set(&["b.ts"])
            ),
            map(&[("a.ts", false)])
        );
    }
    #[test]
    fn later_request_owns_same_value() {
        assert_eq!(
            revert_file_viewed_overlay(&map(&[("a.ts", true)]), &batch(), &set(&["b.ts"])),
            map(&[("a.ts", true)])
        );
    }
    #[test]
    fn failure_leaves_unrelated_path() {
        assert_eq!(
            revert_file_viewed_overlay(&map(&[("c.ts", true)]), &batch(), &set(&["a.ts", "b.ts"])),
            map(&[("c.ts", true)])
        );
    }
    // Pure projections of the four React hook tests.
    #[test]
    fn hook_gives_way_to_post_write_dismissal() {
        let old = states();
        let newer = states();
        let answered =
            answered_paths_after_read(&HashMap::from([("c.ts".to_owned(), 1)]), 2, &set(&[]));
        let settled =
            settle_file_viewed_overlay(&map(&[("c.ts", true)]), Some(&newer), &set(&[]), &answered);
        assert!(!is_file_viewed("c.ts", Some(&old), &settled));
        assert!(is_file_viewed_stale("c.ts", Some(&newer), &settled));
    }
    #[test]
    fn hook_later_read_stays_dismissed() {
        assert!(is_file_viewed_stale("c.ts", Some(&states()), &map(&[])));
    }
    #[test]
    fn hook_holds_while_write_in_flight() {
        let settled = settle_file_viewed_overlay(
            &map(&[("b.ts", true)]),
            Some(&states()),
            &set(&["b.ts"]),
            &set(&[]),
        );
        assert!(is_file_viewed("b.ts", Some(&states()), &settled));
    }
    #[test]
    fn hook_holds_press_made_since_read() {
        let settled = settle_file_viewed_overlay(
            &map(&[("c.ts", true)]),
            Some(&states()),
            &set(&["c.ts"]),
            &set(&["c.ts"]),
        );
        assert!(is_file_viewed("c.ts", Some(&states()), &settled));
        assert!(!is_file_viewed_stale("c.ts", Some(&states()), &settled));
    }
}
