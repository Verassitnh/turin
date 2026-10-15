//! Port of upstream/apps/web/src/components/composerInlineChip.ts

/// Keeps both the recognizable beginning and the extension/end of a long
/// attachment name.
pub fn middle_truncate_attachment_name(name: &str, max_characters: usize) -> String {
    let characters: Vec<char> = name.chars().collect();
    if characters.len() <= max_characters {
        return name.to_owned();
    }
    if max_characters == 0 {
        return String::new();
    }
    if max_characters == 1 {
        return "…".to_owned();
    }
    let available = max_characters - 1;
    let suffix_length = (available - 1).min(if available >= 18 {
        14
    } else {
        available.div_ceil(2)
    });
    let prefix_length = available - suffix_length;
    let suffix: String = if suffix_length == 0 {
        String::new()
    } else {
        characters[characters.len() - suffix_length..]
            .iter()
            .collect()
    };
    let prefix: String = characters[..prefix_length].iter().collect();
    format!("{prefix}…{suffix}")
}

/// Default `maxCharacters`.
pub const DEFAULT_ATTACHMENT_NAME_CHARS: usize = 36;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_short_names_unchanged() {
        assert_eq!(
            middle_truncate_attachment_name("notes.txt", 36),
            "notes.txt"
        );
    }

    #[test]
    fn keeps_the_start_and_filename_extension() {
        let result = middle_truncate_attachment_name(
            "a-very-long-customer-reproduction-screenshot.final.png",
            36,
        );
        assert_eq!(result.chars().count(), 36);
        assert!(result.starts_with("a-very-long-customer"));
        assert!(result.ends_with("final.png"));
        assert!(result.contains('…'));
    }

    #[test]
    fn truncates_unicode_names_without_splitting_a_character() {
        assert_eq!(
            middle_truncate_attachment_name("😀😀😀😀😀😀.png", 8),
            "😀😀😀….png"
        );
    }

    #[test]
    fn honours_very_small_limits() {
        assert_eq!(middle_truncate_attachment_name("notes.txt", 0), "");
        assert_eq!(middle_truncate_attachment_name("notes.txt", 1), "…");
        assert_eq!(middle_truncate_attachment_name("notes.txt", 2), "n…");
    }

    #[test]
    fn keeps_useful_ends_for_multi_dot_names_and_names_without_extensions() {
        assert!(
            middle_truncate_attachment_name("customer.export.final.very-long-name.json", 24)
                .ends_with("long-name.json")
        );
        let result =
            middle_truncate_attachment_name("a-very-long-filename-without-an-extension", 20);
        assert!(result.starts_with("a-ver") && result.ends_with("extension"));
    }
}
