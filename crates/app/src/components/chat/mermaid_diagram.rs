//! Port of upstream/apps/web/src/components/chat/MermaidDiagram.tsx
//!
//! No maintained pure-Rust Mermaid renderer is published. The diagram shows
//! the same error state upstream uses when Mermaid fails: the message, then
//! the source in a wrapping monospace block. The failure is not retryable.

use gpui_kit::{
    AnyElement, App, IntoElement, ParentElement as _, SharedString, Styled as _, div, rems,
};

use crate::theme::ActiveAppTheme;
use crate::theme::tokens::TEXT_XS;

pub const MERMAID_UNAVAILABLE: &str = "No native Mermaid renderer is available.";

/// The error fallback: `Unable to render diagram: {message}` and the source.
pub fn mermaid_diagram(source: impl Into<SharedString>, message: &str, cx: &App) -> AnyElement {
    let theme = cx.app_theme();
    let source = source.into();
    div()
        .w_full()
        .min_w_0()
        .child(
            div()
                .text_size(TEXT_XS.size)
                .text_color(theme.destructive)
                .child(format!("Unable to render diagram: {message}")),
        )
        .child(
            div()
                .mt(rems(0.5))
                .w_full()
                .overflow_hidden()
                .font_family(SharedString::from(theme.fonts.mono.clone()))
                .text_size(TEXT_XS.size)
                .text_color(theme.foreground)
                .child(source),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fallback_message_names_the_missing_renderer() {
        assert!(MERMAID_UNAVAILABLE.contains("Mermaid"));
        assert!(!MERMAID_UNAVAILABLE.is_empty());
    }
}
