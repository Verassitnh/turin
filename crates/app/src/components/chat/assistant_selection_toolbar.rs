//! Port of upstream/apps/web/src/components/chat/AssistantSelectionToolbar.tsx
//!
//! The "Cite" button that floats at the end of a selection inside an assistant message. The
//! DOM half (reading `window.getSelection()`, the `observeSelectionActions` pointer gesture
//! tracking, the document-level Tab handler) is not ported: the host that owns a GPUI text
//! selection supplies the capture as [`AssistantSelectionToolbar::citation_text`] and the
//! position from `resolve_selection_action_position` (`selection_actions.rs`).

use std::rc::Rc;

use gpui_kit::{
    App, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, RenderOnce,
    SharedString, Styled as _, Window, anchored, deferred, div, point, px, rems,
};

use super::selection_actions::SelectionActionPoint;
use super::timeline::assistant_citations::ASSISTANT_CITATION_MAX_TEXT_LENGTH;
use crate::ui::{Button, ButtonSize, ButtonVariant, IconName};

/// `text.length > ASSISTANT_CITATION_MAX_TEXT_LENGTH` (JS length: UTF-16 code units).
pub fn assistant_selection_too_long(text: &str) -> bool {
    text.encode_utf16().count() > ASSISTANT_CITATION_MAX_TEXT_LENGTH
}

pub fn assistant_selection_toolbar_label(too_long: bool) -> &'static str {
    if too_long {
        "Shorten selection"
    } else {
        "Cite"
    }
}

pub fn assistant_selection_toolbar_aria_label(too_long: bool) -> &'static str {
    if too_long {
        "Selection is too long to cite"
    } else {
        "Cite selection in composer"
    }
}

/// The layout effect's clamp: an 8px inset of the viewport on every side.
pub fn clamp_selection_toolbar_position(
    position: SelectionActionPoint,
    toolbar_width: f64,
    toolbar_height: f64,
    viewport_width: f64,
    viewport_height: f64,
) -> SelectionActionPoint {
    SelectionActionPoint {
        x: 8.0_f64.max(position.x.min(viewport_width - toolbar_width - 8.0)),
        y: 8.0_f64.max(position.y.min(viewport_height - toolbar_height - 8.0)),
    }
}

type CiteHandler = Rc<dyn Fn(&mut Window, &mut App) -> bool>;
type DismissHandler = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct AssistantSelectionToolbar {
    /// The captured selection's text; a citation over the limit cannot be made.
    pub citation_text: SharedString,
    pub position: SelectionActionPoint,
    /// `onCite`: whether the composer took the citation.
    pub on_cite: CiteHandler,
    /// Clears the selection state (and the observer's gesture).
    pub on_dismiss: DismissHandler,
    /// Removes the native selection after a successful cite.
    pub on_clear_selection: DismissHandler,
}

impl RenderOnce for AssistantSelectionToolbar {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let too_long = assistant_selection_too_long(&self.citation_text);
        let Self {
            position,
            on_cite,
            on_dismiss,
            on_clear_selection,
            ..
        } = self;
        let dismiss_for_key = on_dismiss.clone();
        deferred(
            anchored()
                .position(point(px(position.x as f32), px(position.y as f32)))
                // `max-w-[calc(100vw-1rem)]` and the 8px inset of the layout effect.
                .snap_to_window_with_margin(px(8.))
                .child(
                    div()
                        .id("assistant-selection-toolbar")
                        .max_w(rems(30.))
                        // `onPointerDown={preventDefault}`: pressing the button must not
                        // collapse the selection it is about to cite.
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_key_down(move |event, window, cx| {
                            cx.stop_propagation();
                            if event.keystroke.key == "escape" {
                                dismiss_for_key(window, cx);
                            }
                        })
                        .child(
                            Button::new("assistant-selection-cite")
                                .variant(ButtonVariant::Glass)
                                .size(ButtonSize::Xs)
                                .icon(IconName::Quote)
                                .label(assistant_selection_toolbar_label(too_long))
                                .disabled(too_long)
                                .on_click(move |_, window, cx| {
                                    if too_long || !on_cite(window, cx) {
                                        return;
                                    }
                                    on_clear_selection(window, cx);
                                    on_dismiss(window, cx);
                                }),
                        ),
                ),
        )
        .with_priority(3)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_selection_over_the_limit_asks_to_be_shortened() {
        let limit = "a".repeat(ASSISTANT_CITATION_MAX_TEXT_LENGTH);
        assert!(!assistant_selection_too_long(&limit));
        assert!(assistant_selection_too_long(&format!("{limit}a")));
        assert_eq!(assistant_selection_toolbar_label(false), "Cite");
        assert_eq!(assistant_selection_toolbar_label(true), "Shorten selection");
        assert_eq!(
            assistant_selection_toolbar_aria_label(true),
            "Selection is too long to cite"
        );
        assert_eq!(
            assistant_selection_toolbar_aria_label(false),
            "Cite selection in composer"
        );
    }

    #[test]
    fn the_limit_counts_utf16_code_units_like_js_length() {
        // One astral character is two UTF-16 units.
        let almost = "a".repeat(ASSISTANT_CITATION_MAX_TEXT_LENGTH - 1);
        assert!(!assistant_selection_too_long(&almost));
        assert!(assistant_selection_too_long(&format!("{almost}😀")));
    }

    #[test]
    fn keeps_the_toolbar_eight_pixels_inside_the_viewport() {
        let clamp = |x, y| {
            clamp_selection_toolbar_position(
                SelectionActionPoint { x, y },
                60.0,
                24.0,
                800.0,
                600.0,
            )
        };
        assert_eq!(clamp(-20.0, -4.0), SelectionActionPoint { x: 8.0, y: 8.0 });
        assert_eq!(
            clamp(790.0, 700.0),
            SelectionActionPoint { x: 732.0, y: 568.0 }
        );
        assert_eq!(
            clamp(100.0, 120.0),
            SelectionActionPoint { x: 100.0, y: 120.0 }
        );
    }
}
