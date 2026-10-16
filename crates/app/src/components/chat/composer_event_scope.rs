//! Port of upstream/apps/web/src/components/chat/composerEventScope.ts
//!
//! `closest` is the fake from the upstream test: an element matches when its
//! selector is one exact candidate in the query.

pub const COMPOSER_FLOATING_LAYER_SELECTOR: &str =
    "[data-composer-drawer-layer=\"true\"],[data-chat-composer-floating-layer=\"true\"]";

pub fn composer_floating_layer_props() -> (&'static str, &'static str) {
    ("data-chat-composer-floating-layer", "true")
}

fn matches(selector: Option<&str>, query: &str) -> bool {
    selector.is_some_and(|selector| query.split(',').any(|candidate| candidate == selector))
}

pub fn is_inside_composer_floating_layer(selector: Option<&str>) -> bool {
    matches(selector, COMPOSER_FLOATING_LAYER_SELECTOR)
}

pub fn is_inside_collapsed_composer_controls(selector: Option<&str>) -> bool {
    matches(selector, "[data-chat-composer-collapsed-controls=\"true\"]")
}

pub fn is_inside_resting_composer_control_scope(selector: Option<&str>) -> bool {
    matches(selector, "[data-chat-composer-resting-controls=\"true\"]")
        || matches(selector, "[data-chat-composer-resting-images=\"true\"]")
        || matches(selector, "[data-composer-context-control]")
        || is_inside_composer_floating_layer(selector)
}

/// `finalFocus`. Returns false, and focuses the composer only when focus is on
/// the body or inside a composer floating layer.
pub fn menu_final_focus(
    active_is_body: bool,
    selector: Option<&str>,
    focus_at_end: impl FnOnce(),
) -> bool {
    if !active_is_body && !is_inside_composer_floating_layer(selector) {
        return false;
    }
    focus_at_end();
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closes_while_focus_is_on_an_open_menu() {
        let mut focused = false;
        assert!(!menu_final_focus(
            false,
            Some("[data-chat-composer-floating-layer=\"true\"]"),
            || focused = true
        ));
        assert!(focused);
    }

    #[test]
    fn closes_while_focus_is_on_an_unmounted_menu() {
        let mut focused = false;
        assert!(!menu_final_focus(true, None, || focused = true));
        assert!(focused);
    }

    #[test]
    fn does_not_steal_focus_from_another_control() {
        let mut focused = false;
        assert!(!menu_final_focus(false, Some("input"), || focused = true));
        assert!(!focused);
    }

    #[test]
    fn recognizes_the_ported_resting_and_collapsed_scopes() {
        assert!(is_inside_resting_composer_control_scope(Some(
            "[data-chat-composer-resting-controls=\"true\"]"
        )));
        assert!(is_inside_resting_composer_control_scope(Some(
            "[data-composer-context-control]"
        )));
        assert!(is_inside_resting_composer_control_scope(Some(
            "[data-chat-composer-resting-images=\"true\"]"
        )));
        assert!(is_inside_composer_floating_layer(Some(
            "[data-chat-composer-floating-layer=\"true\"]"
        )));
        assert!(is_inside_resting_composer_control_scope(Some(
            "[data-chat-composer-floating-layer=\"true\"]"
        )));
        assert!(!is_inside_composer_floating_layer(Some(
            "[data-slot=\"popover-popup\"]"
        )));
        assert!(!is_inside_resting_composer_control_scope(Some(
            "[data-slot=\"popover-popup\"]"
        )));
        assert!(!is_inside_resting_composer_control_scope(None));
        assert!(is_inside_collapsed_composer_controls(Some(
            "[data-chat-composer-collapsed-controls=\"true\"]"
        )));
        assert!(!is_inside_collapsed_composer_controls(None));
    }
}
