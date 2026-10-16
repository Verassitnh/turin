//! Port of the pure rules in upstream/apps/web/src/components/chat/useAssistantCitationTarget.ts.
//!
//! Upstream is a React hook: it keeps the navigation in a ref, reads the timeline
//! and the list through effects, and calls the toast and the scroll. Here the decisions
//! are functions over the values the hook read. The caller applies the step: it
//! requests the history page, expands the turn, shows the toast, or mounts the target.
//! The DOM listeners (keydown, pointerdown, wheel, touchmove) classify their event
//! with the functions below and then dismiss the navigation.
//!
//! There is no upstream test file for this hook; the tests here cover the rules.

use std::collections::HashSet;

use turin_contracts::base_schemas::RunId;

/// Upstream stops asking for earlier history after this many pages.
pub const MAX_CITATION_HISTORY_PAGES: usize = 20;

/// Keys that scroll the timeline. Pressing one while the timeline has focus dismisses a
/// citation that has not finished positioning.
const SCROLL_KEYS: &[&str] = &[
    "ArrowUp",
    "ArrowDown",
    "PageUp",
    "PageDown",
    "Home",
    "End",
    " ",
];

/// The cited message as the timeline holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CitationSource {
    /// No loaded entry has the cited message id.
    Missing,
    /// The cited entry is a user message, or not a message.
    NotAssistant,
    /// The cited assistant message, with its turn when it has one.
    Assistant { run_id: Option<RunId> },
}

/// Earlier-history state the step reads: `loadEarlier` in upstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CitationHistory {
    pub loading: bool,
    /// The cursor the history page is keyed by. Upstream falls back to the first entry
    /// id, then `"first"`.
    pub cursor: Option<String>,
}

/// The navigation's bookkeeping across steps: the history cursors it has asked for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CitationNavigation {
    pub requested_pages: HashSet<String>,
    pub done: bool,
}

/// Inputs to one resolution step.
#[derive(Debug, Clone, Copy)]
pub struct CitationStepInput<'a> {
    pub has_viewport: bool,
    pub history_loading: bool,
    /// `activation.dismissed` for the target.
    pub dismissed: bool,
    pub source: &'a CitationSource,
    /// Whether the cited message has a row in the rendered list.
    pub row_present: bool,
    pub history: Option<&'a CitationHistory>,
    /// The first loaded entry id, for the history cursor fallback.
    pub first_entry_id: Option<&'a str>,
    /// Whether the list has loaded and holds a ref, so the target can mount.
    pub list_ready: bool,
}

/// What the caller does after one resolution step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CitationStep {
    /// Nothing to do yet.
    Wait,
    /// Ask for the earlier history page keyed by `cursor`.
    RequestEarlier { cursor: String },
    /// Show a warning and finish the navigation with `title` and `description`.
    Fail {
        title: &'static str,
        description: &'static str,
    },
    /// Unfold the turn that holds the cited message, so its row can mount.
    ExpandTurn { run_id: RunId },
    /// The target can mount: its row is in the list and the list is ready.
    Mount,
}

/// `resolveAssistantCitationStep`: one pass of the hook's resolution effect.
///
/// Upstream records `requestedPages` before it asks for a page, so a cursor is asked for
/// once. The caller keeps `navigation` across calls.
pub fn resolve_citation_step(
    navigation: &mut CitationNavigation,
    input: CitationStepInput<'_>,
) -> CitationStep {
    if !input.has_viewport || input.history_loading {
        return CitationStep::Wait;
    }
    if navigation.done || input.dismissed {
        return CitationStep::Wait;
    }
    match input.source {
        CitationSource::Missing => {
            let Some(history) = input.history else {
                navigation.done = true;
                return CitationStep::Fail {
                    title: "The cited response is unavailable",
                    description: "It may have been removed. The selected text is still saved in your citation.",
                };
            };
            if history.loading {
                return CitationStep::Wait;
            }
            let cursor = history
                .cursor
                .clone()
                .or_else(|| input.first_entry_id.map(str::to_owned))
                .unwrap_or_else(|| "first".to_owned());
            if navigation.requested_pages.contains(&cursor)
                || navigation.requested_pages.len() >= MAX_CITATION_HISTORY_PAGES
            {
                navigation.done = true;
                return CitationStep::Fail {
                    title: "Could not load the cited response",
                    description: "Load earlier turns, then click the citation to try again. Your saved quote is unchanged.",
                };
            }
            navigation.requested_pages.insert(cursor.clone());
            CitationStep::RequestEarlier { cursor }
        }
        CitationSource::NotAssistant => {
            navigation.done = true;
            CitationStep::Fail {
                title: "The citation does not refer to an assistant response",
                description: "The selected text is still saved in your citation.",
            }
        }
        CitationSource::Assistant { run_id } => {
            if !input.row_present {
                // Unfolding the turn adds the row; the next pass sees it.
                return match run_id {
                    Some(run_id) => CitationStep::ExpandTurn {
                        run_id: run_id.clone(),
                    },
                    None => CitationStep::Wait,
                };
            }
            if input.list_ready {
                CitationStep::Mount
            } else {
                CitationStep::Wait
            }
        }
    }
}

/// The result of dismissing a citation navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CitationDismissal {
    /// The navigation was already dismissed or finished for the pending check, or
    /// does not match this request. Nothing changes.
    pub ignored: bool,
    /// The user moved before the quote was reached, so the caller reports a manual
    /// navigation (ChatView's auto-scroll opt-out).
    pub manual_navigation: bool,
}

/// `dismiss(onlyPending)` in the hook's listener effect.
///
/// `only_pending` dismisses a navigation that has not reached its quote. Escape and
/// navigation by a non-scroll input use `false` and dismiss in every state.
pub fn dismiss_citation(
    navigation_key: Option<&str>,
    request_key: &str,
    scrolled: bool,
    dismissed: bool,
    done: bool,
    only_pending: bool,
) -> CitationDismissal {
    let ignored =
        navigation_key != Some(request_key) || dismissed || (only_pending && (scrolled || done));
    if ignored {
        return CitationDismissal {
            ignored: true,
            manual_navigation: false,
        };
    }
    CitationDismissal {
        ignored: false,
        manual_navigation: !scrolled && !done,
    }
}

/// How a keydown inside or outside the timeline dismisses a citation: `Some(only_pending)`
/// when it should, `None` when it should not. Escape always dismisses; a scroll key
/// dismisses only when the focus is inside the timeline.
pub fn citation_key_dismissal(key: &str, target_in_viewport: bool) -> Option<bool> {
    if key == "Escape" {
        Some(false)
    } else if target_in_viewport && SCROLL_KEYS.contains(&key) {
        Some(true)
    } else {
        None
    }
}

/// A wheel only dismisses on vertical movement.
pub fn citation_wheel_dismisses(delta_y: f64) -> bool {
    delta_y != 0.0
}

/// Whether the target is still positioning: a request exists and has not finished.
pub fn citation_positioning(request_key: Option<&str>, finished_key: Option<&str>) -> bool {
    request_key.is_some_and(|key| finished_key != Some(key))
}

/// The ready target applies only to the request it was made for.
pub fn citation_target_applies(ready_key: Option<&str>, request_key: Option<&str>) -> bool {
    ready_key.is_some() && ready_key == request_key
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(
        source: &'a CitationSource,
        history: Option<&'a CitationHistory>,
    ) -> CitationStepInput<'a> {
        CitationStepInput {
            has_viewport: true,
            history_loading: false,
            dismissed: false,
            source,
            row_present: true,
            history,
            first_entry_id: Some("first-entry"),
            list_ready: true,
        }
    }

    #[test]
    fn waits_without_a_viewport_while_history_loads_or_after_finishing() {
        let source = CitationSource::Missing;
        let mut navigation = CitationNavigation::default();
        let mut no_viewport = input(&source, None);
        no_viewport.has_viewport = false;
        assert_eq!(
            resolve_citation_step(&mut navigation, no_viewport),
            CitationStep::Wait
        );
        let mut loading = input(&source, None);
        loading.history_loading = true;
        assert_eq!(
            resolve_citation_step(&mut navigation, loading),
            CitationStep::Wait
        );
        navigation.done = true;
        assert_eq!(
            resolve_citation_step(&mut navigation, input(&source, None)),
            CitationStep::Wait
        );
    }

    #[test]
    fn a_missing_source_requests_one_earlier_page_per_cursor_then_fails() {
        let source = CitationSource::Missing;
        let history = CitationHistory {
            loading: false,
            cursor: None,
        };
        let mut navigation = CitationNavigation::default();
        assert_eq!(
            resolve_citation_step(&mut navigation, input(&source, Some(&history))),
            CitationStep::RequestEarlier {
                cursor: "first-entry".to_owned()
            }
        );
        // The same cursor is not asked for again while the page is pending.
        assert_eq!(
            resolve_citation_step(&mut navigation, input(&source, Some(&history))),
            CitationStep::Fail {
                title: "Could not load the cited response",
                description: "Load earlier turns, then click the citation to try again. Your saved quote is unchanged.",
            }
        );
        assert!(navigation.done);
    }

    #[test]
    fn a_missing_source_with_no_history_is_unavailable() {
        let source = CitationSource::Missing;
        let mut navigation = CitationNavigation::default();
        assert_eq!(
            resolve_citation_step(&mut navigation, input(&source, None)),
            CitationStep::Fail {
                title: "The cited response is unavailable",
                description: "It may have been removed. The selected text is still saved in your citation.",
            }
        );
    }

    #[test]
    fn the_page_budget_caps_history_requests() {
        let source = CitationSource::Missing;
        let history = CitationHistory {
            loading: false,
            cursor: Some("older".to_owned()),
        };
        let mut navigation = CitationNavigation {
            requested_pages: (0..MAX_CITATION_HISTORY_PAGES)
                .map(|i| format!("p{i}"))
                .collect(),
            done: false,
        };
        assert!(matches!(
            resolve_citation_step(&mut navigation, input(&source, Some(&history))),
            CitationStep::Fail { .. }
        ));
    }

    #[test]
    fn a_citation_to_a_user_message_fails_with_its_own_warning() {
        let source = CitationSource::NotAssistant;
        let mut navigation = CitationNavigation::default();
        assert_eq!(
            resolve_citation_step(&mut navigation, input(&source, None)),
            CitationStep::Fail {
                title: "The citation does not refer to an assistant response",
                description: "The selected text is still saved in your citation.",
            }
        );
    }

    #[test]
    fn an_assistant_message_outside_its_turn_expands_the_turn_then_mounts() {
        let run = RunId::new("run-1").expect("run id");
        let source = CitationSource::Assistant {
            run_id: Some(run.clone()),
        };
        let mut navigation = CitationNavigation::default();
        let mut folded = input(&source, None);
        folded.row_present = false;
        assert_eq!(
            resolve_citation_step(&mut navigation, folded),
            CitationStep::ExpandTurn { run_id: run }
        );
        let mut waiting = input(&source, None);
        waiting.list_ready = false;
        assert_eq!(
            resolve_citation_step(&mut navigation, waiting),
            CitationStep::Wait
        );
        assert_eq!(
            resolve_citation_step(&mut navigation, input(&source, None)),
            CitationStep::Mount
        );
    }

    #[test]
    fn dismissal_reports_manual_navigation_only_before_the_quote_is_reached() {
        let before = dismiss_citation(Some("k"), "k", false, false, false, false);
        assert_eq!(
            before,
            CitationDismissal {
                ignored: false,
                manual_navigation: true
            }
        );
        // Escape after the quote was reached dismisses without reporting manual navigation.
        let escaped = dismiss_citation(Some("k"), "k", true, false, false, false);
        assert_eq!(
            escaped,
            CitationDismissal {
                ignored: false,
                manual_navigation: false
            }
        );
        // A pending-only dismissal leaves a navigation that already reached its quote alone.
        let reached = dismiss_citation(Some("k"), "k", true, false, false, true);
        assert!(reached.ignored);
        assert!(dismiss_citation(Some("other"), "k", false, false, false, false).ignored);
        assert!(dismiss_citation(Some("k"), "k", false, true, false, false).ignored);
    }

    #[test]
    fn keys_and_wheel_classify_how_a_citation_is_dismissed() {
        assert_eq!(citation_key_dismissal("Escape", false), Some(false));
        assert_eq!(citation_key_dismissal("PageDown", true), Some(true));
        assert_eq!(citation_key_dismissal(" ", true), Some(true));
        assert_eq!(citation_key_dismissal("PageDown", false), None);
        assert_eq!(citation_key_dismissal("a", true), None);
        assert!(citation_wheel_dismisses(-3.0));
        assert!(!citation_wheel_dismisses(0.0));
    }

    #[test]
    fn positioning_and_targets_follow_the_request_key() {
        assert!(citation_positioning(Some("a"), None));
        assert!(citation_positioning(Some("a"), Some("b")));
        assert!(!citation_positioning(Some("a"), Some("a")));
        assert!(!citation_positioning(None, None));
        assert!(citation_target_applies(Some("a"), Some("a")));
        assert!(!citation_target_applies(Some("a"), Some("b")));
        assert!(!citation_target_applies(None, None));
    }
}
