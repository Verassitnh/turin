//! Port of upstream/apps/web/src/components/chat/useAssistantCitationTarget.ts
//!
//! The hook's effects become a controller the timeline steps whenever the
//! request, rows, or history change. It still fetches, unfolds, and mounts
//! the source before scrolling, and it still drops a pending reveal when the
//! reader takes over.

use std::collections::HashSet;

use super::timeline::assistant_citations::AssistantCitation;

/// `CitationHistoryPage`.
#[derive(Clone, Debug)]
pub struct CitationHistoryPage {
    pub loading: bool,
    pub cursor: Option<String>,
}

/// One timeline entry the controller can recognise as the cited message.
#[derive(Clone, Debug)]
pub struct CitationEntryView {
    pub id: String,
    pub message_id: Option<String>,
    pub assistant: bool,
    pub run_id: Option<String>,
}

/// One rendered row. Only message rows carry `message_id`.
#[derive(Clone, Debug)]
pub struct CitationRowView {
    pub id: String,
    pub message_id: Option<String>,
}

/// `AssistantCitationRequest`.
#[derive(Clone, Debug)]
pub struct AssistantCitationRequest {
    pub citation: AssistantCitation,
    pub key: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CitationTargetEffect {
    ManualNavigation,
    LoadEarlier,
    ExpandTurn(String),
    Toast {
        title: &'static str,
        description: &'static str,
    },
    CancelScroll,
}

struct Activation {
    scrolled: bool,
    dismissed: bool,
}

struct Navigation {
    key: String,
    message_id: String,
    requested_pages: HashSet<String>,
    done: bool,
    activation: Activation,
}

/// What the timeline knows on one step. `viewport` is whether the list is mounted.
#[derive(Clone, Debug)]
pub struct CitationTargetInput {
    pub request: Option<AssistantCitationRequest>,
    pub entries: Vec<CitationEntryView>,
    pub rows: Vec<CitationRowView>,
    pub viewport: bool,
    pub history_loading: bool,
    pub load_earlier: Option<CitationHistoryPage>,
    pub list_loaded: bool,
    pub list_present: bool,
}

/// `useAssistantCitationTarget` without the DOM listeners. Call [`sync`] when
/// inputs change and [`dismiss`] for Escape, a pending scroll gesture, or a
/// pointer that takes over.
#[derive(Default)]
pub struct AssistantCitationController {
    navigation: Option<Navigation>,
    ready: bool,
    finished_key: Option<String>,
}

impl AssistantCitationController {
    pub fn sync(&mut self, input: &CitationTargetInput) -> Vec<CitationTargetEffect> {
        let mut effects = Vec::new();
        let request_key = input.request.as_ref().map(|request| request.key.as_str());
        if self
            .navigation
            .as_ref()
            .is_some_and(|navigation| Some(navigation.key.as_str()) != request_key)
            && let Some(navigation) = self.navigation.as_mut()
        {
            navigation.activation.dismissed = true;
            effects.push(CitationTargetEffect::CancelScroll);
        }
        let Some(request) = input.request.as_ref() else {
            self.navigation = None;
            self.ready = false;
            self.finished_key = None;
            return effects;
        };
        if self
            .navigation
            .as_ref()
            .is_none_or(|navigation| navigation.key != request.key)
        {
            self.navigation = Some(Navigation {
                key: request.key.clone(),
                message_id: request.citation.message_id.clone(),
                requested_pages: HashSet::new(),
                done: false,
                activation: Activation {
                    scrolled: false,
                    dismissed: false,
                },
            });
            self.ready = false;
            effects.push(CitationTargetEffect::ManualNavigation);
        }
        if !input.viewport || input.history_loading {
            return effects;
        }
        let Some(navigation) = self.navigation.as_mut() else {
            return effects;
        };
        if navigation.done || navigation.activation.dismissed {
            return effects;
        }
        let source = input
            .entries
            .iter()
            .find(|entry| entry.message_id.as_deref() == Some(navigation.message_id.as_str()));
        let Some(source) = source else {
            if let Some(page) = &input.load_earlier {
                if page.loading {
                    return effects;
                }
                let cursor = page
                    .cursor
                    .clone()
                    .or_else(|| input.entries.first().map(|entry| entry.id.clone()))
                    .unwrap_or_else(|| "first".into());
                if navigation.requested_pages.contains(&cursor)
                    || navigation.requested_pages.len() >= 20
                {
                    fail(
                        navigation,
                        &mut self.finished_key,
                        "Could not load the cited response",
                        "Load earlier turns, then click the citation to try again. Your saved quote is unchanged.",
                        &mut effects,
                    );
                    return effects;
                }
                navigation.requested_pages.insert(cursor);
                effects.push(CitationTargetEffect::LoadEarlier);
                return effects;
            }
            fail(
                navigation,
                &mut self.finished_key,
                "The cited response is unavailable",
                "It may have been removed. The selected text is still saved in your citation.",
                &mut effects,
            );
            return effects;
        };
        if !source.assistant {
            fail(
                navigation,
                &mut self.finished_key,
                "The citation does not refer to an assistant response",
                "The selected text is still saved in your citation.",
                &mut effects,
            );
            return effects;
        }
        let in_rows = input
            .rows
            .iter()
            .any(|row| row.message_id.as_deref() == Some(navigation.message_id.as_str()));
        if !in_rows {
            if let Some(run_id) = source.run_id.clone() {
                effects.push(CitationTargetEffect::ExpandTurn(run_id));
            }
            return effects;
        }
        if input.list_loaded && input.list_present {
            self.ready = true;
        }
        effects
    }

    /// Escape dismisses outright. A wheel, key, or pointer dismisses only while
    /// the reveal has not scrolled yet (`only_pending`).
    pub fn dismiss(&mut self, request_key: &str, only_pending: bool) -> Vec<CitationTargetEffect> {
        let mut effects = Vec::new();
        let Some(navigation) = self.navigation.as_mut() else {
            return effects;
        };
        if navigation.key != request_key {
            return effects;
        }
        if navigation.activation.dismissed
            || (only_pending && (navigation.activation.scrolled || navigation.done))
        {
            return effects;
        }
        navigation.activation.dismissed = true;
        effects.push(CitationTargetEffect::CancelScroll);
        if !navigation.activation.scrolled && !navigation.done {
            effects.push(CitationTargetEffect::ManualNavigation);
        }
        navigation.done = true;
        self.ready = false;
        self.finished_key = Some(request_key.to_owned());
        effects
    }

    /// The source observer may mark the reveal scrolled once the row is in view.
    pub fn mark_scrolled(&mut self) {
        if let Some(navigation) = self.navigation.as_mut() {
            navigation.activation.scrolled = true;
            navigation.done = true;
            self.finished_key = Some(navigation.key.clone());
        }
    }

    pub fn scrolled(&self) -> bool {
        self.navigation
            .as_ref()
            .is_some_and(|navigation| navigation.activation.scrolled)
    }

    pub fn target<'a>(
        &'a self,
        request: Option<&'a AssistantCitationRequest>,
    ) -> Option<&'a AssistantCitationRequest> {
        let request = request?;
        if self.ready
            && self
                .navigation
                .as_ref()
                .is_some_and(|navigation| navigation.key == request.key)
        {
            Some(request)
        } else {
            None
        }
    }

    pub fn positioning(&self, request: Option<&AssistantCitationRequest>) -> bool {
        match request {
            Some(request) => self.finished_key.as_deref() != Some(request.key.as_str()),
            None => false,
        }
    }

    /// `alwaysRender`: the source row stays mounted while it is being positioned.
    pub fn always_render_row_id<'a>(
        &self,
        request: Option<&AssistantCitationRequest>,
        rows: &'a [CitationRowView],
    ) -> Option<&'a str> {
        let request = self.target(request)?;
        if !self.positioning(Some(request)) {
            return None;
        }
        rows.iter().find_map(|row| {
            (row.message_id.as_deref() == Some(request.citation.message_id.as_str()))
                .then_some(row.id.as_str())
        })
    }
}

fn fail(
    navigation: &mut Navigation,
    finished_key: &mut Option<String>,
    title: &'static str,
    description: &'static str,
    effects: &mut Vec<CitationTargetEffect>,
) {
    navigation.done = true;
    *finished_key = Some(navigation.key.clone());
    effects.push(CitationTargetEffect::Toast { title, description });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn citation(message_id: &str) -> AssistantCitation {
        AssistantCitation {
            version: 1,
            environment_id: "environment".into(),
            thread_id: "thread".into(),
            message_id: message_id.into(),
            text: "hello".into(),
            comment: None,
            start: 0,
            end: 5,
            prefix: String::new(),
            suffix: String::new(),
        }
    }

    fn request(key: &str) -> AssistantCitationRequest {
        AssistantCitationRequest {
            citation: citation("source"),
            key: key.into(),
        }
    }

    fn entry(
        id: &str,
        message_id: Option<&str>,
        assistant: bool,
        run_id: Option<&str>,
    ) -> CitationEntryView {
        CitationEntryView {
            id: id.into(),
            message_id: message_id.map(str::to_owned),
            assistant,
            run_id: run_id.map(str::to_owned),
        }
    }

    fn row(id: &str, message_id: Option<&str>) -> CitationRowView {
        CitationRowView {
            id: id.into(),
            message_id: message_id.map(str::to_owned),
        }
    }

    fn input(request: Option<AssistantCitationRequest>) -> CitationTargetInput {
        CitationTargetInput {
            request,
            entries: vec![entry("e1", Some("source"), true, Some("run-1"))],
            rows: vec![row("row-source", Some("source"))],
            viewport: true,
            history_loading: false,
            load_earlier: None,
            list_loaded: true,
            list_present: true,
        }
    }

    #[test]
    fn becomes_ready_once_the_assistant_row_is_mounted() {
        let mut controller = AssistantCitationController::default();
        let request = request("activation-one");
        let effects = controller.sync(&input(Some(request.clone())));
        assert_eq!(effects, vec![CitationTargetEffect::ManualNavigation]);
        assert!(controller.target(Some(&request)).is_some());
        assert!(controller.positioning(Some(&request)));
        let rows = input(None).rows;
        assert_eq!(
            controller.always_render_row_id(Some(&request), &rows),
            Some("row-source")
        );
    }

    #[test]
    fn waits_while_history_is_loading_or_the_list_has_no_viewport() {
        let mut controller = AssistantCitationController::default();
        let mut waiting = input(Some(request("a")));
        waiting.history_loading = true;
        controller.sync(&waiting);
        assert!(controller.target(waiting.request.as_ref()).is_none());
        waiting.history_loading = false;
        waiting.viewport = false;
        controller.sync(&waiting);
        assert!(controller.target(waiting.request.as_ref()).is_none());
    }

    #[test]
    fn asks_for_an_earlier_page_then_fails_when_the_cursor_repeats() {
        let mut controller = AssistantCitationController::default();
        let mut missing = input(Some(request("a")));
        missing.entries.clear();
        missing.rows.clear();
        missing.load_earlier = Some(CitationHistoryPage {
            loading: false,
            cursor: Some("c1".into()),
        });
        let effects = controller.sync(&missing);
        assert!(effects.contains(&CitationTargetEffect::LoadEarlier));
        let effects = controller.sync(&missing);
        assert!(effects.iter().any(|effect| matches!(
            effect,
            CitationTargetEffect::Toast {
                title: "Could not load the cited response",
                ..
            }
        )));
        assert!(!controller.positioning(missing.request.as_ref()));
    }

    #[test]
    fn fails_when_the_cited_response_is_gone_and_history_is_exhausted() {
        let mut controller = AssistantCitationController::default();
        let mut missing = input(Some(request("a")));
        missing.entries.clear();
        let effects = controller.sync(&missing);
        assert!(effects.iter().any(|effect| matches!(
            effect,
            CitationTargetEffect::Toast {
                title: "The cited response is unavailable",
                ..
            }
        )));
    }

    #[test]
    fn rejects_a_citation_that_does_not_name_an_assistant_response() {
        let mut controller = AssistantCitationController::default();
        let mut wrong = input(Some(request("a")));
        wrong.entries[0].assistant = false;
        let effects = controller.sync(&wrong);
        assert!(effects.iter().any(|effect| matches!(
            effect,
            CitationTargetEffect::Toast {
                title: "The citation does not refer to an assistant response",
                ..
            }
        )));
    }

    #[test]
    fn expands_a_folded_turn_before_the_row_exists() {
        let mut controller = AssistantCitationController::default();
        let mut folded = input(Some(request("a")));
        folded.rows.clear();
        let effects = controller.sync(&folded);
        assert_eq!(
            effects,
            vec![
                CitationTargetEffect::ManualNavigation,
                CitationTargetEffect::ExpandTurn("run-1".into()),
            ]
        );
        assert!(controller.target(folded.request.as_ref()).is_none());
    }

    #[test]
    fn escape_dismisses_a_scrolled_reveal_and_a_gesture_does_not() {
        let mut controller = AssistantCitationController::default();
        let request = request("a");
        controller.sync(&input(Some(request.clone())));
        controller.mark_scrolled();
        assert!(controller.dismiss("a", true).is_empty());
        let effects = controller.dismiss("a", false);
        assert_eq!(effects, vec![CitationTargetEffect::CancelScroll]);
        assert!(controller.target(Some(&request)).is_none());
    }

    #[test]
    fn a_pending_gesture_cancels_the_scroll_and_opts_out_of_auto_follow() {
        let mut controller = AssistantCitationController::default();
        controller.sync(&input(Some(request("a"))));
        let effects = controller.dismiss("a", true);
        assert_eq!(
            effects,
            vec![
                CitationTargetEffect::CancelScroll,
                CitationTargetEffect::ManualNavigation,
            ]
        );
        assert!(!controller.scrolled());
    }

    #[test]
    fn switching_activations_dismisses_the_previous_one() {
        let mut controller = AssistantCitationController::default();
        controller.sync(&input(Some(request("first"))));
        let effects = controller.sync(&input(Some(request("second"))));
        assert!(effects.contains(&CitationTargetEffect::CancelScroll));
        assert!(effects.contains(&CitationTargetEffect::ManualNavigation));
    }
}

// Main's pure decision rules for the same hook (S2b's controller above steps
// the live navigation; these are the stateless per-step functions and their tests).
pub mod pure_rules {
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
        let ignored = navigation_key != Some(request_key)
            || dismissed
            || (only_pending && (scrolled || done));
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
}
