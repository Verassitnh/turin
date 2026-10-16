//! Port of upstream/apps/web/src/hooks/useNavigateBack.ts.

use crate::shell::app_route::NavigationStack;

/// `useNavigateBack`.
pub fn navigate_back(stack: &mut NavigationStack) {
    stack.navigate_back();
}

/// `useEscapeToGoBack` — returns whether Escape should navigate back.
pub fn escape_should_navigate_back(default_prevented: bool, key: &str, repeat: bool) -> bool {
    !default_prevented && !repeat && key == "Escape"
}
