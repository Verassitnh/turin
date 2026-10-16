//! Port of upstream/apps/web/src/components/chat/ComposerPromptLengthValidation.tsx
//!
//! The error line under the prompt when the draft cannot be sent
//! (`getComposerPromptLengthValidationMessage`, or the submission error).

use gpui_kit::{AnyElement, IntoElement as _, ParentElement as _, SharedString, Styled as _, div};

use crate::theme::AppTheme;
use crate::theme::tokens::TEXT_XS;

/// `if (!message) return null`.
fn visible_message(message: Option<&str>) -> Option<&str> {
    message.filter(|message| !message.is_empty())
}

/// `px-3 pb-2 text-xs text-destructive sm:px-4`; nothing without a message.
pub fn composer_prompt_length_validation(
    message: Option<&str>,
    theme: &AppTheme,
) -> Option<AnyElement> {
    let message = visible_message(message)?;
    Some(
        div()
            .px_4()
            .pb_2()
            .text_size(TEXT_XS.size)
            .line_height(TEXT_XS.line_height)
            .text_color(theme.destructive)
            .child(SharedString::from(message.to_owned()))
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::visible_message;

    #[test]
    fn renders_nothing_for_a_missing_or_empty_message() {
        assert_eq!(visible_message(None), None);
        assert_eq!(visible_message(Some("")), None);
        assert_eq!(visible_message(Some("Too long")), Some("Too long"));
    }
}
