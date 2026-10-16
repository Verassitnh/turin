//! Port of upstream/apps/web/src/components/chat/ThreadDetailsControl.tsx

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ClickEvent, CursorStyle, ElementId, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, ParentElement as _, RenderOnce, StatefulInteractiveElement as _, Styled as _,
    Window, div, px, rems,
};

use super::thread_details_panel_styles::{
    THREAD_DETAILS_ROW_GAP, THREAD_DETAILS_ROW_HEIGHT, THREAD_DETAILS_ROW_PADDING_X,
};
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_LG, RADIUS_MD, TEXT_SM, tw};
use crate::theme::{ActiveAppTheme, AppTheme};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ThreadDetailsControlPart {
    #[default]
    Row,
    Select,
    Primary,
    Secondary,
    Icon,
    Meta,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ThreadDetailsControlTone {
    #[default]
    Default,
    Muted,
    Primary,
    Destructive,
}

type ClickHandler = Option<Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>>;

#[derive(IntoElement)]
pub struct ThreadDetailsControl {
    id: ElementId,
    part: ThreadDetailsControlPart,
    tone: ThreadDetailsControlTone,
    multiline: bool,
    disabled: bool,
    label: Option<gpui_kit::SharedString>,
    children: gpui_kit::AnyElement,
    on_click: ClickHandler,
}

impl ThreadDetailsControl {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            part: ThreadDetailsControlPart::Row,
            tone: ThreadDetailsControlTone::Default,
            multiline: false,
            disabled: false,
            label: None,
            children: div().into_any_element(),
            on_click: None,
        }
    }

    pub fn part(mut self, part: ThreadDetailsControlPart) -> Self {
        self.part = part;
        self
    }

    pub fn tone(mut self, tone: ThreadDetailsControlTone) -> Self {
        self.tone = tone;
        self
    }

    #[allow(dead_code)]
    pub fn multiline(mut self, multiline: bool) -> Self {
        self.multiline = multiline;
        self
    }

    #[allow(dead_code)]
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    #[allow(dead_code)]
    pub fn label(mut self, label: impl Into<gpui_kit::SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn child(mut self, child: impl IntoElement) -> Self {
        self.children = child.into_any_element();
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

fn row_hover_bg(theme: &AppTheme, dark: bool) -> Hsla {
    if dark {
        with_alpha(tw::WHITE, 0.08).into()
    } else {
        with_alpha(theme.foreground, 0.035).into()
    }
}

impl RenderOnce for ThreadDetailsControl {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let dark = theme.is_dark();
        let hover = row_hover_bg(&theme, dark);
        let (height, width, radius, justify) = match self.part {
            ThreadDetailsControlPart::Row | ThreadDetailsControlPart::Select => {
                (THREAD_DETAILS_ROW_HEIGHT, None, RADIUS_LG, "start")
            }
            ThreadDetailsControlPart::Primary => {
                (THREAD_DETAILS_ROW_HEIGHT, None, RADIUS_LG, "start")
            }
            ThreadDetailsControlPart::Secondary => (
                THREAD_DETAILS_ROW_HEIGHT,
                Some(px(32.)),
                RADIUS_LG,
                "center",
            ),
            ThreadDetailsControlPart::Icon => (rems(1.5), Some(px(24.)), RADIUS_MD, "center"),
            ThreadDetailsControlPart::Meta => (rems(1.5), None, RADIUS_MD, "center"),
        };
        let text_color = match self.tone {
            ThreadDetailsControlTone::Default => with_alpha(theme.foreground, 0.85).into(),
            ThreadDetailsControlTone::Muted => with_alpha(theme.muted_foreground, 0.7).into(),
            ThreadDetailsControlTone::Primary => theme.primary,
            ThreadDetailsControlTone::Destructive => theme.destructive,
        };
        let mut control = div()
            .id(self.id)
            .flex()
            .items_center()
            .gap(THREAD_DETAILS_ROW_GAP)
            .px(THREAD_DETAILS_ROW_PADDING_X)
            .h(height)
            .rounded(radius)
            .text_size(TEXT_SM.size)
            .line_height(TEXT_SM.line_height)
            .font_weight(FontWeight::MEDIUM)
            .text_color(text_color)
            .cursor(CursorStyle::PointingHand)
            .when(self.part == ThreadDetailsControlPart::Row, |this| {
                this.w_full()
            })
            .when(self.part == ThreadDetailsControlPart::Primary, |this| {
                this.flex_1().rounded_r_none()
            })
            .when(self.part == ThreadDetailsControlPart::Secondary, |this| {
                this.rounded_l_none().px_0()
            })
            .when(self.multiline, |this| {
                this.h_auto().min_h(height).py(px(3.))
            })
            .when(self.disabled, |this| {
                this.opacity(0.64).cursor(CursorStyle::default())
            })
            .child(self.children);
        if let Some(w) = width {
            control = control.w(w).justify_center();
        } else if justify == "start" {
            control = control.justify_start();
        }
        if !self.disabled {
            control = control.hover(|s| s.bg(hover)).active(|s| s.bg(hover));
        }
        if let Some(handler) = self.on_click {
            control = control.on_click(move |event, window, cx| handler(event, window, cx));
        }
        if let Some(label) = self.label {
            control = control.child(label);
        }
        control
    }
}

/// `THREAD_DETAILS_PANEL_SPLIT_SEPARATOR_CLASS`
#[allow(dead_code)]
pub fn thread_details_split_separator(theme: &AppTheme) -> impl IntoElement {
    div()
        .h(rems(1.))
        .w(px(1.))
        .flex_shrink_0()
        .bg(with_alpha(theme.border, 0.65))
}

/// Split row container (`THREAD_DETAILS_PANEL_SPLIT_GROUP_CLASS`).
pub fn thread_details_split_group(children: Vec<gpui_kit::AnyElement>) -> gpui_kit::Div {
    div()
        .flex()
        .w_full()
        .items_center()
        .rounded(RADIUS_LG)
        .children(children)
}
