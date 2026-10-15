//! Port of upstream/apps/web/src/components/ui/collapsible-section-header.tsx
//!
//! `CollapsibleSectionHeader`: label, a hairline filling the row, an
//! optional accessory and a chevron that points down while expanded
//! (`rotate-180` when collapsed). Callers own the section state. Colors
//! resolve inside the sidebar token scope, where upstream renders it.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, CursorStyle, ElementId, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, RenderOnce, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px, rems,
};

use super::{Icon, IconName};
use crate::components::drag_region::no_drag;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_MD, TEXT_3XS, TEXT_XS, tw};
use crate::theme::{ActiveAppTheme, AppTheme};

/// `tones`: label and line colors.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CollapsibleSectionTone {
    /// `text-sidebar-muted-foreground/60`, line `bg-sidebar-border/60`.
    #[default]
    Muted,
    /// `text-blue-600 dark:text-blue-400`, line `bg-blue-500/20 dark:bg-blue-400/15`.
    Info,
    /// `text-sidebar-foreground/80`, line `bg-sidebar-foreground/25`.
    Emphasized,
    /// `text-primary`, line `bg-primary/50`.
    Accent,
}

impl CollapsibleSectionTone {
    fn colors(self, theme: &AppTheme) -> (Hsla, Hsla) {
        let scope = theme.sidebar_scope();
        let dark = theme.is_dark();
        match self {
            Self::Muted => (
                with_alpha(scope.sidebar_muted_foreground, 0.6).into(),
                with_alpha(scope.sidebar_border, 0.6).into(),
            ),
            Self::Info => (
                if dark {
                    tw::BLUE_400
                } else {
                    crate::theme::color::hex(0x155dfc)
                }
                .into(),
                if dark {
                    with_alpha(tw::BLUE_400, 0.15)
                } else {
                    with_alpha(tw::BLUE_500, 0.2)
                }
                .into(),
            ),
            Self::Emphasized => (
                with_alpha(scope.sidebar_foreground, 0.8).into(),
                with_alpha(scope.sidebar_foreground, 0.25).into(),
            ),
            Self::Accent => (theme.primary, with_alpha(theme.primary, 0.5).into()),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CollapsibleSectionVariant {
    /// `px-2`.
    #[default]
    Default,
    /// `ps-2 pe-2.5`.
    Panel,
}

#[derive(IntoElement)]
pub struct CollapsibleSectionHeader {
    id: ElementId,
    label: SharedString,
    expanded: bool,
    tone: CollapsibleSectionTone,
    variant: CollapsibleSectionVariant,
    accessory: Option<AnyElement>,
    on_click: Option<super::sidebar::ClickHandler>,
}

impl CollapsibleSectionHeader {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>, expanded: bool) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            expanded,
            tone: CollapsibleSectionTone::Muted,
            variant: CollapsibleSectionVariant::Default,
            accessory: None,
            on_click: None,
        }
    }

    pub fn tone(mut self, tone: CollapsibleSectionTone) -> Self {
        self.tone = tone;
        self
    }

    pub fn variant(mut self, variant: CollapsibleSectionVariant) -> Self {
        self.variant = variant;
        self
    }

    pub fn accessory(mut self, accessory: impl IntoElement) -> Self {
        self.accessory = Some(accessory.into_any_element());
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

impl RenderOnce for CollapsibleSectionHeader {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let (label, line) = self.tone.colors(theme);
        div()
            .id(self.id)
            .on_mouse_down(MouseButton::Left, no_drag)
            .flex()
            .h_8()
            .w_full()
            .items_center()
            .gap_2()
            .rounded(RADIUS_MD)
            .cursor(CursorStyle::PointingHand)
            .text_size(TEXT_XS.size)
            .line_height(TEXT_XS.line_height)
            .font_weight(FontWeight::MEDIUM)
            .text_color(label)
            .map(|this| match self.variant {
                CollapsibleSectionVariant::Default => this.px_2(),
                CollapsibleSectionVariant::Panel => this.pl_2().pr(rems(0.625)),
            })
            .child(div().flex_shrink_0().child(self.label))
            .child(div().h(px(1.)).min_w_2().flex_1().bg(line))
            .children(self.accessory)
            .child(
                Icon::new(if self.expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronUp
                })
                .size(rems(0.75))
                .color(label),
            )
            .when_some(self.on_click, |this, handler| {
                this.on_click(move |event, window, cx| handler(event, window, cx))
            })
    }
}

/// `SectionHeaderStatus`: `shrink-0 text-3xs leading-none text-destructive-foreground`.
#[derive(IntoElement)]
pub struct SectionHeaderStatus(pub SharedString);

impl RenderOnce for SectionHeaderStatus {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        div()
            .flex_shrink_0()
            .text_size(TEXT_3XS.size)
            .line_height(TEXT_3XS.size)
            .text_color(cx.app_theme().destructive_foreground)
            .child(self.0)
    }
}
