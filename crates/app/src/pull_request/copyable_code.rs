//! Port of upstream/apps/web/src/components/pullRequest/PullRequestCopyableCode.tsx: a branch name
//! or command that copies itself on click and says "Copied" in its own place for 1.6s.
//!
//! The `transition-opacity` cross-fade is an instant swap here (GPUI style refinements do not
//! animate).

use std::time::Duration;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ClipboardItem, CursorStyle, ElementId, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window, div, px,
};

use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::ui::{Tooltip, TooltipVariant};

/// `timeout: 1600`.
const COPIED_FOR: Duration = Duration::from_millis(1600);

#[derive(IntoElement)]
pub struct PullRequestCopyableCode {
    id: ElementId,
    value: SharedString,
    copy_label: SharedString,
    /// `className="ml-auto font-mono"`.
    ml_auto: bool,
}

impl PullRequestCopyableCode {
    pub fn new(
        id: impl Into<ElementId>,
        value: impl Into<SharedString>,
        copy_label: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            value: value.into(),
            copy_label: copy_label.into(),
            ml_auto: false,
        }
    }

    pub fn ml_auto(mut self) -> Self {
        self.ml_auto = true;
        self
    }
}

impl RenderOnce for PullRequestCopyableCode {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let copied = window.use_keyed_state(
            SharedString::from(format!("copyable-code:{:?}", self.id)),
            cx,
            |_, _| false,
        );
        let is_copied = *copied.read(cx);
        let value = self.value.clone();
        let tooltip = SharedString::from(format!(
            "{}: {}",
            if is_copied { "Copied" } else { self.copy_label.as_ref() },
            self.value
        ));
        let group = SharedString::from(format!("copyable:{:?}", self.id));
        div()
            .id(self.id)
            .group(group.clone())
            .relative()
            .flex()
            .w_auto()
            .min_w_0()
            .max_w_full()
            .flex_shrink_0()
            .when(self.ml_auto, |this| this.ml_auto())
            .rounded(px(4.))
            .px_1()
            .py(px(2.))
            .cursor(CursorStyle::PointingHand)
            .hover(|style| {
                style
                    .bg(with_alpha(theme.accent, 0.45))
                    .text_color(theme.foreground)
            })
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(if is_copied { SharedString::from("Copied") } else { value.clone() }),
            )
            .on_click({
                let copied = copied.clone();
                move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(value.to_string()));
                    copied.update(cx, |state, cx| {
                        *state = true;
                        cx.notify();
                    });
                    let copied = copied.downgrade();
                    cx.spawn(async move |cx| {
                        cx.background_executor().timer(COPIED_FOR).await;
                        copied
                            .update(cx, |state, cx| {
                                *state = false;
                                cx.notify();
                            })
                            .ok();
                    })
                    .detach();
                }
            })
            .tooltip(move |window, cx| {
                Tooltip::new(tooltip.clone())
                    .variant(TooltipVariant::Code)
                    .build(window, cx)
            })
    }
}
