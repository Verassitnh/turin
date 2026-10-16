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
