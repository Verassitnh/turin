//! `foldPastedText` from upstream/apps/web/src/components/chat/ChatComposer.tsx
//! (the decision only; the composer stages the file and raises the toast).
//!
//! A large clipboard text (32 KiB or more, or one that would push the prompt
//! over the send limit) becomes a `pasted-text.txt` attachment instead of
//! landing inline, so an agent can read it selectively. The paste-as-text
//! shortcut (⌘⇧V) keeps one paste inline.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use turin_contracts::chat_attachment::{
    PROVIDER_SEND_TURN_MAX_ATTACHMENTS, PROVIDER_SEND_TURN_MAX_INPUT_CHARS,
};

use crate::composer::attachment_files::format_attachment_size;
use crate::state::helpers::text_paste::{
    PastedTextDisposition, PastedTextDispositionInput, TextPasteLimitInput, TextSelection,
    next_pasted_text_file_name, pasted_text_disposition, would_text_paste_exceed_limit,
};

/// `text/plain;charset=utf-8`, the folded file's type.
pub const PASTED_TEXT_MIME_TYPE: &str = "text/plain;charset=utf-8";

/// How long the paste-as-text shortcut keeps the next paste inline (upstream's
/// `Date.now() + 1_000`, bridging the key event and the paste event).
pub const PASTE_AS_TEXT_WINDOW: Duration = Duration::from_millis(1_000);

/// Armed by the editor on ⌘⇧V, consumed by the next paste.
#[derive(Clone, Default)]
pub struct PasteAsTextShortcut(Rc<Cell<Option<Instant>>>);

impl PasteAsTextShortcut {
    pub fn arm(&self, now: Instant) {
        self.0.set(Some(now + PASTE_AS_TEXT_WINDOW));
    }

    /// `Date.now() <= pasteAsTextShortcutUntilRef.current`, then reset.
    pub fn take(&self, now: Instant) -> bool {
        self.0.take().is_some_and(|until| now <= until)
    }

    pub fn disarm(&self) {
        self.0.set(None);
    }
}

pub struct PasteFoldInput<'a> {
    pub text: &'a str,
    pub bypass_auto_attachment: bool,
    /// An agent question's literal answer keeps pasted text, whatever its size.
    pub is_literal: bool,
    /// UTF-16 length of the prompt and the selection the paste replaces.
    pub value_length: usize,
    pub selection: (usize, usize),
    /// Draft attachments already taken, including images still compressing.
    pub reserved_attachments: usize,
    /// `fileStagingLimit`: `None` when the server takes no file uploads.
    pub staging_limit: Option<u64>,
    /// File names already in the draft, so folded names stay distinct.
    pub existing_file_names: &'a [String],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasteFold {
    /// Let the editor paste the text.
    Inline,
    /// Stage the text as `name`, then toast "Large paste attached as …".
    Attach { name: String },
    /// Swallow the paste and say why.
    Refuse {
        title: &'static str,
        description: &'static str,
    },
}

pub fn fold_pasted_text(input: &PasteFoldInput<'_>) -> PasteFold {
    if input.is_literal {
        return PasteFold::Inline;
    }
    let would_exceed_input_limit = would_text_paste_exceed_limit(&TextPasteLimitInput {
        value_length: input.value_length as i64,
        selection: TextSelection {
            start: input.selection.0 as i64,
            end: input.selection.1 as i64,
        },
        text_length: turin_contracts::js_len(input.text) as i64,
        max_length: PROVIDER_SEND_TURN_MAX_INPUT_CHARS as i64,
    });
    let disposition = pasted_text_disposition(&PastedTextDispositionInput {
        text: input.text,
        can_attach: true,
        bypass_auto_attachment: input.bypass_auto_attachment,
        would_exceed_input_limit,
    });
    if disposition != PastedTextDisposition::Attachment {
        return PasteFold::Inline;
    }
    let has_slot = input.reserved_attachments < PROVIDER_SEND_TURN_MAX_ATTACHMENTS;
    let limit = match input.staging_limit {
        Some(limit) if has_slot => limit,
        _ => {
            return if would_exceed_input_limit {
                PasteFold::Refuse {
                    title: "Pasted text is too large for this message",
                    description: "Remove some text or an attachment, then paste again.",
                }
            } else {
                PasteFold::Inline
            };
        }
    };
    if input.text.len() as u64 > limit {
        return if would_exceed_input_limit {
            PasteFold::Refuse {
                title: "Pasted text is too large to attach",
                description: "Reduce the clipboard contents or save a smaller excerpt as a file.",
            }
        } else {
            PasteFold::Inline
        };
    }
    PasteFold::Attach {
        name: next_pasted_text_file_name(input.existing_file_names),
    }
}

/// The info toast's text after a fold: `(title, description)`.
pub fn large_paste_toast(name: &str, size_bytes: u64) -> (String, String) {
    (
        format!("Large paste attached as {name}"),
        format!(
            "{} · Use ⌘⇧V to keep a large paste inline.",
            format_attachment_size(size_bytes)
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::helpers::text_paste::PASTED_TEXT_ATTACHMENT_THRESHOLD_BYTES as THRESHOLD;

    fn input<'a>(text: &'a str, names: &'a [String]) -> PasteFoldInput<'a> {
        PasteFoldInput {
            text,
            bypass_auto_attachment: false,
            is_literal: false,
            value_length: 0,
            selection: (0, 0),
            reserved_attachments: 0,
            staging_limit: Some(10 * 1024 * 1024),
            existing_file_names: names,
        }
    }

    #[test]
    fn small_text_stays_inline() {
        let text = "x".repeat(THRESHOLD - 1);
        assert_eq!(fold_pasted_text(&input(&text, &[])), PasteFold::Inline);
    }

    #[test]
    fn large_text_becomes_a_pasted_text_attachment() {
        let text = "x".repeat(THRESHOLD);
        assert_eq!(
            fold_pasted_text(&input(&text, &[])),
            PasteFold::Attach {
                name: "pasted-text.txt".into()
            }
        );
    }

    #[test]
    fn a_second_large_paste_gets_the_next_name() {
        let text = "x".repeat(THRESHOLD);
        let names = ["pasted-text.txt".to_owned()];
        assert_eq!(
            fold_pasted_text(&input(&text, &names)),
            PasteFold::Attach {
                name: "pasted-text-2.txt".into()
            }
        );
    }

    #[test]
    fn the_paste_as_text_shortcut_keeps_it_inline() {
        let text = "x".repeat(THRESHOLD);
        let mut paste = input(&text, &[]);
        paste.bypass_auto_attachment = true;
        assert_eq!(fold_pasted_text(&paste), PasteFold::Inline);
    }

    #[test]
    fn an_answer_to_an_agent_question_keeps_the_text() {
        let text = "x".repeat(THRESHOLD);
        let mut paste = input(&text, &[]);
        paste.is_literal = true;
        assert_eq!(fold_pasted_text(&paste), PasteFold::Inline);
    }

    #[test]
    fn a_full_draft_or_no_upload_support_keeps_text_inline_when_it_fits() {
        let text = "x".repeat(THRESHOLD);
        let mut full = input(&text, &[]);
        full.reserved_attachments = PROVIDER_SEND_TURN_MAX_ATTACHMENTS;
        assert_eq!(fold_pasted_text(&full), PasteFold::Inline);
        let mut no_uploads = input(&text, &[]);
        no_uploads.staging_limit = None;
        assert_eq!(fold_pasted_text(&no_uploads), PasteFold::Inline);
    }

    #[test]
    fn text_that_cannot_stay_inline_or_attach_is_refused() {
        let text = "x".repeat(10);
        let mut over = input(&text, &[]);
        over.value_length = PROVIDER_SEND_TURN_MAX_INPUT_CHARS;
        over.staging_limit = None;
        assert!(matches!(
            fold_pasted_text(&over),
            PasteFold::Refuse {
                title: "Pasted text is too large for this message",
                ..
            }
        ));
        // A selection that is replaced frees room: the paste fits again.
        over.selection = (0, 10);
        assert_eq!(fold_pasted_text(&over), PasteFold::Inline);
        // Over the limit and over the file limit.
        let mut too_big = input(&text, &[]);
        too_big.value_length = PROVIDER_SEND_TURN_MAX_INPUT_CHARS;
        too_big.staging_limit = Some(4);
        assert!(matches!(
            fold_pasted_text(&too_big),
            PasteFold::Refuse {
                title: "Pasted text is too large to attach",
                ..
            }
        ));
    }

    #[test]
    fn a_small_paste_that_overflows_the_prompt_is_attached() {
        let mut overflow = input("tiny", &[]);
        overflow.value_length = PROVIDER_SEND_TURN_MAX_INPUT_CHARS;
        assert!(matches!(
            fold_pasted_text(&overflow),
            PasteFold::Attach { .. }
        ));
    }

    #[test]
    fn the_shortcut_applies_to_the_next_paste_only_and_expires() {
        let shortcut = PasteAsTextShortcut::default();
        let now = Instant::now();
        assert!(!shortcut.take(now));
        shortcut.arm(now);
        assert!(shortcut.take(now + Duration::from_millis(900)));
        assert!(!shortcut.take(now + Duration::from_millis(901)));
        shortcut.arm(now);
        assert!(!shortcut.take(now + Duration::from_millis(1_001)));
    }

    #[test]
    fn the_toast_names_the_file_and_the_shortcut() {
        let (title, description) = large_paste_toast("pasted-text.txt", 40 * 1024);
        assert_eq!(title, "Large paste attached as pasted-text.txt");
        assert!(description.ends_with("Use ⌘⇧V to keep a large paste inline."));
    }
}
