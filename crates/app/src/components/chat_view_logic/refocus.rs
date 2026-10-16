//! Port of upstream/apps/web/src/components/ChatView.logic.ts (`shouldRefocusComposerOnWindowFocus`).

/// Minimal DOM surface the refocus rule inspects (upstream `activeElement`).
pub trait FocusableElement {
    fn tag_name(&self) -> &str;
    fn is_content_editable(&self) -> bool;
    fn role_attribute(&self) -> Option<&str>;
    fn closest_matches(&self, selector: &str) -> bool;
}

pub fn should_refocus_composer_on_window_focus(element: Option<&dyn FocusableElement>) -> bool {
    let Some(element) = element else {
        return true;
    };
    if element.tag_name() == "BODY" {
        return true;
    }
    if matches!(
        element.tag_name(),
        "INPUT" | "TEXTAREA" | "SELECT" | "IFRAME" | "WEBVIEW"
    ) || element.is_content_editable()
        || element.role_attribute() == Some("textbox")
    {
        return false;
    }
    !element.closest_matches("[role=\"dialog\"]")
        && !element.closest_matches("[role=\"alertdialog\"]")
        && !element.closest_matches("[data-slot$=\"-popup\"]")
        && !element.closest_matches("[data-terminal-owner]")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestElement {
        tag: &'static str,
        editable: bool,
        role: Option<&'static str>,
        within: Option<&'static str>,
    }

    impl FocusableElement for TestElement {
        fn tag_name(&self) -> &str {
            self.tag
        }

        fn is_content_editable(&self) -> bool {
            self.editable
        }

        fn role_attribute(&self) -> Option<&str> {
            self.role
        }

        fn closest_matches(&self, selector: &str) -> bool {
            self.within.is_some_and(|within| selector.contains(within))
        }
    }

    fn element(
        tag: &'static str,
        editable: bool,
        role: Option<&'static str>,
        within: Option<&'static str>,
    ) -> TestElement {
        TestElement {
            tag,
            editable,
            role,
            within,
        }
    }

    #[test]
    fn refocuses_when_nothing_or_the_body_holds_focus() {
        assert!(should_refocus_composer_on_window_focus(None));
        assert!(should_refocus_composer_on_window_focus(Some(&element(
            "BODY", false, None, None
        ))));
    }

    #[test]
    fn refocuses_away_from_a_plain_button() {
        assert!(should_refocus_composer_on_window_focus(Some(&element(
            "BUTTON", false, None, None
        ))));
    }

    #[test]
    fn leaves_other_text_fields_alone() {
        assert!(!should_refocus_composer_on_window_focus(Some(&element(
            "INPUT", false, None, None
        ))));
        assert!(!should_refocus_composer_on_window_focus(Some(&element(
            "TEXTAREA", false, None, None
        ))));
        assert!(!should_refocus_composer_on_window_focus(Some(&element(
            "DIV", true, None, None
        ))));
        assert!(!should_refocus_composer_on_window_focus(Some(&element(
            "DIV",
            false,
            Some("textbox"),
            None
        ))));
    }

    #[test]
    fn leaves_iframe_and_webview_previews_alone() {
        assert!(!should_refocus_composer_on_window_focus(Some(&element(
            "IFRAME", false, None, None
        ))));
        assert!(!should_refocus_composer_on_window_focus(Some(&element(
            "WEBVIEW", false, None, None
        ))));
    }

    #[test]
    fn leaves_a_focused_terminal_alone() {
        assert!(!should_refocus_composer_on_window_focus(Some(&element(
            "BUTTON",
            false,
            None,
            Some("data-terminal-owner"),
        ))));
    }

    #[test]
    fn leaves_focus_inside_a_dialog_or_popup_alone() {
        assert!(!should_refocus_composer_on_window_focus(Some(&element(
            "BUTTON",
            false,
            None,
            Some("dialog")
        ))));
        assert!(!should_refocus_composer_on_window_focus(Some(&element(
            "BUTTON",
            false,
            None,
            Some("-popup")
        ))));
    }
}
