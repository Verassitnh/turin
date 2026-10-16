//! Port of upstream/apps/web/src/components/sidebar/mainAppLocation.ts

use std::sync::Mutex;

/// Settings, Usage, and Pull Requests replace the sidebar utility row with a
/// Back button. Everything else is the main app. Legacy `/projects/<key>` links
/// redirect into settings, so they count too and are never remembered.
pub fn is_sidebar_utility_page(pathname: &str) -> bool {
    pathname == "/settings"
        || pathname.starts_with("/settings/")
        || pathname.starts_with("/projects/")
        || pathname == "/usage"
        || pathname == "/pull-requests"
}

static MAIN_APP_HREF: Mutex<Option<String>> = Mutex::new(None);

/// Records the latest main app URL so Back can return there.
pub fn track_main_app_location(pathname: &str, href: &str) {
    if !is_sidebar_utility_page(pathname) {
        *MAIN_APP_HREF.lock().unwrap() = Some(href.to_owned());
    }
}

/// Leaves a utility page for the last main app URL, or the thread list when the
/// app was opened directly on a utility page.
pub fn navigate_to_main_app_href() -> String {
    MAIN_APP_HREF
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_else(|| "/".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utility_pages_are_detected() {
        assert!(is_sidebar_utility_page("/settings"));
        assert!(is_sidebar_utility_page("/settings/github"));
        assert!(is_sidebar_utility_page("/projects/foo"));
        assert!(is_sidebar_utility_page("/usage"));
        assert!(is_sidebar_utility_page("/pull-requests"));
        assert!(!is_sidebar_utility_page("/"));
        assert!(!is_sidebar_utility_page("/thread/abc"));
    }

    #[test]
    fn remembers_last_main_app_href() {
        *MAIN_APP_HREF.lock().unwrap() = None;
        track_main_app_location("/thread/1", "/thread/1?panel=diff");
        assert_eq!(navigate_to_main_app_href(), "/thread/1?panel=diff");
        track_main_app_location("/settings", "/settings");
        assert_eq!(navigate_to_main_app_href(), "/thread/1?panel=diff");
    }
}
