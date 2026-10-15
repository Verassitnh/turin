//! Port of upstream/apps/web/src/components/chat/composerMenuHighlight.ts

/// `resolveComposerMenuActiveItemId`: keep the highlighted item only while the
/// query is unchanged and the item is still listed; otherwise the top result.
pub fn resolve_composer_menu_active_item_id<'a>(
    items: &'a [&'a str],
    highlighted_item_id: Option<&str>,
    current_search_key: Option<&str>,
    highlighted_search_key: Option<&str>,
) -> Option<&'a str> {
    if items.is_empty() {
        return None;
    }
    if current_search_key == highlighted_search_key
        && let Some(highlighted) = highlighted_item_id
        && let Some(found) = items.iter().find(|id| **id == highlighted)
    {
        return Some(found);
    }
    items.first().copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ITEMS: [&str; 3] = ["top", "second", "third"];

    #[test]
    fn defaults_to_the_first_item_when_nothing_is_highlighted() {
        assert_eq!(
            resolve_composer_menu_active_item_id(&ITEMS, None, Some("skill:u"), None),
            Some("top")
        );
    }

    #[test]
    fn preserves_the_highlighted_item_within_the_same_query() {
        assert_eq!(
            resolve_composer_menu_active_item_id(
                &ITEMS,
                Some("second"),
                Some("skill:u"),
                Some("skill:u")
            ),
            Some("second")
        );
    }

    #[test]
    fn resets_to_the_top_result_when_the_query_changes() {
        assert_eq!(
            resolve_composer_menu_active_item_id(
                &ITEMS,
                Some("second"),
                Some("skill:ui"),
                Some("skill:u")
            ),
            Some("top")
        );
    }

    #[test]
    fn falls_back_to_the_first_item_when_the_highlighted_item_disappears() {
        assert_eq!(
            resolve_composer_menu_active_item_id(
                &ITEMS,
                Some("missing"),
                Some("skill:ui"),
                Some("skill:ui")
            ),
            Some("top")
        );
    }

    #[test]
    fn clears_while_async_results_are_empty_and_resolves_against_restored_results() {
        let cleared = resolve_composer_menu_active_item_id(
            &[],
            Some("second"),
            Some("path:src"),
            Some("path:src"),
        );
        assert_eq!(cleared, None);
        assert_eq!(
            resolve_composer_menu_active_item_id(
                &["new-result", "second"],
                cleared,
                Some("path:src"),
                Some("path:src")
            ),
            Some("new-result")
        );
    }
}
