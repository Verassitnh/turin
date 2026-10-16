//! Port of upstream/apps/web/src/components/chat/ThreadErrorBanner.tsx

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

const KEY_SEP: char = '\0';

#[allow(dead_code)]
static SESSION_DISMISSED: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

pub fn get_thread_error_banner_key(thread_key: &str, error: Option<&str>) -> Option<String> {
    error.map(|message| format!("{thread_key}{KEY_SEP}{message}"))
}

pub fn should_show_thread_error_banner(
    thread_key: &str,
    error: Option<&str>,
    is_dismissed: bool,
) -> bool {
    get_thread_error_banner_key(thread_key, error).is_some() && !is_dismissed
}

#[allow(dead_code)]
pub fn dismiss_thread_error_banner_for_session(banner_key: Option<&str>) {
    if let Some(key) = banner_key
        && let Ok(mut set) = SESSION_DISMISSED.lock()
    {
        set.insert(key.to_owned());
    }
}

#[allow(dead_code)]
pub fn is_thread_error_banner_dismissed_for_session(banner_key: Option<&str>) -> bool {
    banner_key.is_some_and(|key| {
        SESSION_DISMISSED
            .lock()
            .ok()
            .is_some_and(|set| set.contains(key))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset() {
        if let Ok(mut set) = SESSION_DISMISSED.lock() {
            set.clear();
        }
    }

    #[test]
    fn stays_hidden_after_its_current_error_is_dismissed() {
        reset(); // isolate session store
        let banner_key = get_thread_error_banner_key("env:thread-a", Some("Aborted"));
        dismiss_thread_error_banner_for_session(banner_key.as_deref());
        assert!(!should_show_thread_error_banner(
            "env:thread-a",
            Some("Aborted"),
            is_thread_error_banner_dismissed_for_session(banner_key.as_deref()),
        ));
    }

    #[test]
    fn reappears_when_a_new_error_arrives_on_the_same_thread() {
        reset(); // isolate session store
        dismiss_thread_error_banner_for_session(
            get_thread_error_banner_key("env:thread-b", Some("Turn failed")).as_deref(),
        );
        let new_error_key = get_thread_error_banner_key("env:thread-b", Some("Provider crashed"));
        assert!(!is_thread_error_banner_dismissed_for_session(
            new_error_key.as_deref()
        ));
        assert!(should_show_thread_error_banner(
            "env:thread-b",
            Some("Provider crashed"),
            is_thread_error_banner_dismissed_for_session(new_error_key.as_deref()),
        ));
    }

    #[test]
    fn scopes_dismissals_to_the_thread_that_dismissed_them() {
        reset(); // isolate session store
        dismiss_thread_error_banner_for_session(
            get_thread_error_banner_key("env:thread-c", Some("Aborted")).as_deref(),
        );
        let other_thread_key = get_thread_error_banner_key("env:other-thread", Some("Aborted"));
        assert!(!is_thread_error_banner_dismissed_for_session(
            other_thread_key.as_deref()
        ));
        assert!(should_show_thread_error_banner(
            "env:other-thread",
            Some("Aborted"),
            is_thread_error_banner_dismissed_for_session(other_thread_key.as_deref()),
        ));
    }

    #[test]
    fn keeps_a_dismissal_across_visiting_threads_with_no_error() {
        reset();
        let banner_key = get_thread_error_banner_key("env:thread-d", Some("Aborted"));
        dismiss_thread_error_banner_for_session(banner_key.as_deref());
        assert!(!should_show_thread_error_banner(
            "env:thread-d",
            None,
            false
        ));
        assert!(is_thread_error_banner_dismissed_for_session(
            banner_key.as_deref()
        ));
        assert!(!should_show_thread_error_banner(
            "env:thread-d",
            Some("Aborted"),
            is_thread_error_banner_dismissed_for_session(banner_key.as_deref()),
        ));
    }

    #[test]
    fn never_shows_a_null_error() {
        assert!(!should_show_thread_error_banner(
            "env:thread-e",
            None,
            false
        ));
    }
}
