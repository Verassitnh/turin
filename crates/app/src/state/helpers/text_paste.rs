//! Port of upstream/packages/client-runtime/src/textPaste.ts
//!
//! Selections and lengths are UTF-16 code unit offsets, as in the JS text fields these helpers
//! serve.

use std::collections::HashSet;

use turin_contracts::js_len;

pub const PASTED_TEXT_ATTACHMENT_THRESHOLD_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PastedTextDisposition {
    Attachment,
    Inline,
}

/// `Pick<KeyboardEvent, "key" | "metaKey" | "ctrlKey" | "altKey" | "shiftKey">`
#[derive(Debug, Clone, Copy)]
pub struct PasteShortcutEvent<'a> {
    pub key: &'a str,
    pub meta_key: bool,
    pub ctrl_key: bool,
    pub alt_key: bool,
    pub shift_key: bool,
}

pub fn is_paste_as_text_shortcut(event: &PasteShortcutEvent<'_>, mac_platform: bool) -> bool {
    event.key.to_lowercase() == "v"
        && event.shift_key
        && !event.alt_key
        && if mac_platform {
            event.meta_key && !event.ctrl_key
        } else {
            event.ctrl_key && !event.meta_key
        }
}

#[derive(Debug, Clone, Copy)]
pub struct PastedTextDispositionInput<'a> {
    pub text: &'a str,
    pub can_attach: bool,
    pub bypass_auto_attachment: bool,
    pub would_exceed_input_limit: bool,
}

/// Large clipboard text becomes a file so an agent can inspect it selectively.
/// The threshold is byte-based: character counts substantially understate the
/// context cost of some Unicode-heavy clipboard contents.
pub fn pasted_text_disposition(input: &PastedTextDispositionInput<'_>) -> PastedTextDisposition {
    if input.bypass_auto_attachment || !input.can_attach || input.text.is_empty() {
        return PastedTextDisposition::Inline;
    }
    if input.would_exceed_input_limit
        || js_len(input.text) >= PASTED_TEXT_ATTACHMENT_THRESHOLD_BYTES
        || input.text.len() >= PASTED_TEXT_ATTACHMENT_THRESHOLD_BYTES
    {
        PastedTextDisposition::Attachment
    } else {
        PastedTextDisposition::Inline
    }
}

/// Stable, human-readable names when a draft contains several folded pastes.
pub fn next_pasted_text_file_name(existing_names: &[String]) -> String {
    let names: HashSet<String> = existing_names
        .iter()
        .map(|name| name.to_lowercase())
        .collect();
    if !names.contains("pasted-text.txt") {
        return "pasted-text.txt".to_owned();
    }
    let mut sequence = 2;
    loop {
        let candidate = format!("pasted-text-{sequence}.txt");
        if !names.contains(&candidate) {
            return candidate;
        }
        sequence += 1;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextSelection {
    pub start: i64,
    pub end: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplacedTextSelection {
    pub value: String,
    pub cursor: i64,
}

pub fn replace_text_selection(
    value: &str,
    selection: TextSelection,
    text: &str,
) -> ReplacedTextSelection {
    let units: Vec<u16> = value.encode_utf16().collect();
    let length = units.len() as i64;
    let start = selection.start.min(length).max(0);
    let end = selection.end.min(length).max(start);
    // A selection that lands inside a surrogate pair cannot be spliced without producing a lone
    // surrogate, which a Rust string cannot hold; those halves become U+FFFD.
    let before = String::from_utf16_lossy(&units[..start as usize]);
    let after = String::from_utf16_lossy(&units[end as usize..]);
    ReplacedTextSelection {
        value: format!("{before}{text}{after}"),
        cursor: start + js_len(text) as i64,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TextPasteLimitInput {
    pub value_length: i64,
    pub selection: TextSelection,
    pub text_length: i64,
    pub max_length: i64,
}

pub fn would_text_paste_exceed_limit(input: &TextPasteLimitInput) -> bool {
    let start = input.selection.start.min(input.value_length).max(0);
    let end = input.selection.end.min(input.value_length).max(start);
    input.value_length - (end - start) + input.text_length > input.max_length
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disposition(text: &str, can_attach: bool) -> PastedTextDispositionInput<'_> {
        PastedTextDispositionInput {
            text,
            can_attach,
            bypass_auto_attachment: false,
            would_exceed_input_limit: false,
        }
    }

    #[test]
    fn keeps_ordinary_text_inline_and_folds_at_the_32_kib_boundary() {
        let below = "x".repeat(PASTED_TEXT_ATTACHMENT_THRESHOLD_BYTES - 1);
        let at = "x".repeat(PASTED_TEXT_ATTACHMENT_THRESHOLD_BYTES);
        assert_eq!(
            pasted_text_disposition(&disposition(&below, true)),
            PastedTextDisposition::Inline
        );
        assert_eq!(
            pasted_text_disposition(&disposition(&at, true)),
            PastedTextDisposition::Attachment
        );
    }

    #[test]
    fn measures_utf8_bytes_instead_of_utf16_characters() {
        let text = "🙂".repeat(PASTED_TEXT_ATTACHMENT_THRESHOLD_BYTES / 4);
        assert!(js_len(&text) < PASTED_TEXT_ATTACHMENT_THRESHOLD_BYTES);
        assert_eq!(
            pasted_text_disposition(&disposition(&text, true)),
            PastedTextDisposition::Attachment
        );
    }

    #[test]
    fn keeps_text_inline_for_the_explicit_bypass_or_without_attachment_support() {
        let text = "x".repeat(PASTED_TEXT_ATTACHMENT_THRESHOLD_BYTES);
        assert_eq!(
            pasted_text_disposition(&PastedTextDispositionInput {
                bypass_auto_attachment: true,
                ..disposition(&text, true)
            }),
            PastedTextDisposition::Inline
        );
        assert_eq!(
            pasted_text_disposition(&disposition(&text, false)),
            PastedTextDisposition::Inline
        );
    }

    #[test]
    fn folds_a_smaller_paste_when_the_resulting_prompt_would_exceed_the_input_limit() {
        assert_eq!(
            pasted_text_disposition(&PastedTextDispositionInput {
                would_exceed_input_limit: true,
                ..disposition("small paste", true)
            }),
            PastedTextDisposition::Attachment
        );
    }

    #[test]
    fn uses_the_first_available_readable_sequence() {
        assert_eq!(next_pasted_text_file_name(&[]), "pasted-text.txt");
        assert_eq!(
            next_pasted_text_file_name(&[
                "PASTED-TEXT.TXT".to_owned(),
                "pasted-text-2.txt".to_owned()
            ]),
            "pasted-text-3.txt"
        );
    }

    #[test]
    fn accepts_the_platform_modifier_with_shift() {
        for (meta_key, ctrl_key, mac_platform) in [(true, false, true), (false, true, false)] {
            assert!(is_paste_as_text_shortcut(
                &PasteShortcutEvent {
                    key: "V",
                    meta_key,
                    ctrl_key,
                    shift_key: true,
                    alt_key: false,
                },
                mac_platform
            ));
        }
    }

    #[test]
    fn does_not_claim_ordinary_or_alternate_paste_chords() {
        assert!(!is_paste_as_text_shortcut(
            &PasteShortcutEvent {
                key: "v",
                meta_key: true,
                ctrl_key: false,
                shift_key: false,
                alt_key: false,
            },
            true
        ));
        assert!(!is_paste_as_text_shortcut(
            &PasteShortcutEvent {
                key: "v",
                meta_key: true,
                ctrl_key: false,
                shift_key: true,
                alt_key: true,
            },
            true
        ));
    }

    #[test]
    fn rejects_the_other_platforms_modifier() {
        let event = PasteShortcutEvent {
            key: "v",
            meta_key: false,
            ctrl_key: true,
            shift_key: true,
            alt_key: false,
        };
        assert!(!is_paste_as_text_shortcut(&event, true));
        assert!(!is_paste_as_text_shortcut(
            &PasteShortcutEvent {
                meta_key: true,
                ctrl_key: false,
                ..event
            },
            false
        ));
    }

    #[test]
    fn replaces_the_selected_range_and_returns_the_collapsed_cursor() {
        assert_eq!(
            replace_text_selection(
                "before old after",
                TextSelection { start: 7, end: 10 },
                "new"
            ),
            ReplacedTextSelection {
                value: "before new after".to_owned(),
                cursor: 10
            }
        );
    }

    #[test]
    fn clamps_stale_native_selections() {
        assert_eq!(
            replace_text_selection("abc", TextSelection { start: 20, end: 30 }, "!"),
            ReplacedTextSelection {
                value: "abc!".to_owned(),
                cursor: 4
            }
        );
    }

    #[test]
    fn measures_the_replacement_value_after_removing_the_selected_range() {
        let input = |text_length| TextPasteLimitInput {
            value_length: 100,
            selection: TextSelection { start: 40, end: 80 },
            text_length,
            max_length: 100,
        };
        assert!(!would_text_paste_exceed_limit(&input(30)));
        assert!(would_text_paste_exceed_limit(&input(41)));
    }
}
