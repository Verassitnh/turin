//! Port of upstream/apps/web/src/composerPlaceholder.ts

pub const DISCONNECTED_COMPOSER_PLACEHOLDER: &str =
    "Ask for changes, send follow-ups, or attach images";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disconnected_placeholder_matches_upstream() {
        assert_eq!(
            DISCONNECTED_COMPOSER_PLACEHOLDER,
            "Ask for changes, send follow-ups, or attach images"
        );
    }
}
