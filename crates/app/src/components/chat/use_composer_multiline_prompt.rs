//! Port of upstream/apps/web/src/components/chat/useComposerMultilinePrompt.ts
//!
//! `measureComposerMultilinePrompt` reads the composer body's computed style
//! and a DOM range around the editor's content. Those reads are
//! [`PromptBody`]; the view implements it over its layout. The hook is
//! [`ComposerMultilinePrompt`]: the view calls [`remeasure`] from its
//! resize/content observers, and a hidden editor keeps the previous answer.
//!
//! [`remeasure`]: ComposerMultilinePrompt::remeasure

/// What the measurement reads from the composer body.
pub trait PromptBody {
    /// `[data-testid="composer-editor"]`'s `clientWidth`, or `None` when the
    /// body has no editor.
    fn editor_client_width(&self) -> Option<f64>;
    fn body_client_width(&self) -> f64;
    /// `paddingLeft` and `paddingRight` of the body.
    fn body_horizontal_padding(&self) -> (f64, f64);
    /// The editor's computed `lineHeight`.
    fn editor_line_height(&self) -> f64;
    /// `(width, height)` of the range selecting the editor's content.
    fn content_bounds(&self) -> (f64, f64);
}

/// `measureComposerMultilinePrompt`. `None` when the editor is missing or
/// hidden (zero width), so callers keep the previous measurement.
pub fn measure_composer_multiline_prompt(body: &dyn PromptBody) -> Option<bool> {
    let editor_width = body.editor_client_width()?;
    if editor_width == 0.0 {
        return None;
    }
    let (padding_left, padding_right) = body.body_horizontal_padding();
    let expanded_width = body.body_client_width() - padding_left - padding_right;
    let line_height = body.editor_line_height();
    let (width, height) = body.content_bounds();

    // Measure content, not the editor's minimum height. While resting, the
    // prompt is unwrapped: compare its width with the expanded row so that
    // moving the actions inline cannot make collapse and expansion oscillate.
    Some(height > line_height + 1.0 || width > expanded_width + 1.0)
}

/// `useComposerMultilinePrompt`'s state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ComposerMultilinePrompt {
    is_multiline: bool,
}

impl ComposerMultilinePrompt {
    pub fn is_multiline(&self) -> bool {
        self.is_multiline
    }

    /// The hook's `measure`: stores the answer unless the editor is hidden.
    pub fn remeasure(&mut self, body: &dyn PromptBody) -> bool {
        if let Some(next) = measure_composer_multiline_prompt(body) {
            self.is_multiline = next;
        }
        self.is_multiline
    }
}

#[cfg(test)]
#[path = "use_composer_multiline_prompt_tests.rs"]
mod tests;
