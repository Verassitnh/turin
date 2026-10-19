//! Ports of upstream/apps/web/src/components/chat/threadDetailsPanelStyles.ts
//! and ThreadDetailsControl.tsx.
//!
//! Panel controls keep one density at every breakpoint, independent of
//! toolbar button sizes: 32px rows with `rounded-lg`, 13px medium text at
//! `foreground/80`, and a `black/3.5%` (white/8% dark) hover and pressed
//! surface. The split group (primary action + chevron) shares one hover
//! surface across both halves.
//!
//! Port of upstream/apps/web/src/components/chat/ThreadDetailsControl.tsx.

// A faithful port keeps upstream's whole API (PORTING rule 3); the hosts wire
// the rest as the composer, thread details and settings tasks land.
#![allow(dead_code)]

use crate::theme::color::solid_over;
use std::rc::Rc;

use gpui_kit::component::Selectable;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, CursorStyle, ElementId, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, ParentElement, RenderOnce, StatefulInteractiveElement as _, Styled, Window, div,
    px, rems,
};

use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_LG, RADIUS_MD, TEXT_XS, tw};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::icon::{Icon, IconName};
use crate::ui::tooltip::Tooltip;

/// `THREAD_DETAILS_PANEL_ROW_SURFACE_CLASS` hover / pressed fill.
pub fn panel_hover_surface(theme: &AppTheme) -> Hsla {
    if theme.is_dark() {
        with_alpha(tw::WHITE, 0.08).into()
    } else {
        with_alpha(tw::BLACK, 0.035).into()
    }
}

/// `THREAD_DETAILS_PANEL_ICON_CLASS`: `size-4 shrink-0 text-muted-foreground`.
pub fn panel_icon(name: IconName, theme: &AppTheme) -> Icon {
    Icon::new(name).size(rems(1.)).color(theme.muted_foreground)
}

/// `THREAD_DETAILS_PANEL_SPLIT_SEPARATOR_CLASS`: `h-4 w-px shrink-0 bg-border/65`.
pub fn panel_split_separator(theme: &AppTheme) -> AnyElement {
    div()
        .h(rems(1.))
        .w(px(1.))
        .flex_shrink_0()
        .bg(solid_over(theme.border, 0.65, theme.background))
        .into_any_element()
}

/// `THREAD_DETAILS_PANEL_SPLIT_GROUP_CLASS` frame: `flex w-full items-center
/// rounded-lg` with a hover surface over both halves.
pub fn panel_split_group(
    id: impl Into<ElementId>,
    active: bool,
    theme: &AppTheme,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let hover = panel_hover_surface(theme);
    div()
        .id(id)
        .flex()
        .w_full()
        .items_center()
        .rounded(RADIUS_LG)
        .when(active, move |this| this.bg(hover))
        .hover(move |style| style.bg(hover))
}

/// The `part` of upstream's `parts` table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThreadDetailsPart {
    /// `THREAD_DETAILS_PANEL_ROW_CLASS`: a full-width row.
    #[default]
    Row,
    /// A row with a trailing chevron cell (`…SELECT_ROW_CLASS`).
    Select,
    /// The primary half of a split group (`…SPLIT_PRIMARY_CLASS`).
    Primary,
    /// The chevron half (`…SPLIT_SECONDARY_CLASS`): `w-8`.
    Secondary,
    /// A 24px icon action (`…ICON_ACTION_CLASS`).
    Icon,
    /// A small text control on a row's second line (`…META_ACTION_CLASS`).
    Meta,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThreadDetailsTone {
    #[default]
    Default,
    Muted,
    Primary,
    Destructive,
}

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// `<ThreadDetailsControl part tone multiline>`.
#[derive(IntoElement)]
pub struct ThreadDetailsControl {
    id: ElementId,
    part: ThreadDetailsPart,
    tone: ThreadDetailsTone,
    multiline: bool,
    disabled: bool,
    selected: bool,
    on_click: Option<ClickHandler>,
    tooltip: Option<Tooltip>,
    children: Vec<AnyElement>,
}

impl ThreadDetailsControl {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            part: ThreadDetailsPart::Row,
            tone: ThreadDetailsTone::Default,
            multiline: false,
            disabled: false,
            selected: false,
            on_click: None,
            tooltip: None,
            children: Vec::new(),
        }
    }

    pub fn part(mut self, part: ThreadDetailsPart) -> Self {
        self.part = part;
        self
    }

    pub fn tone(mut self, tone: ThreadDetailsTone) -> Self {
        self.tone = tone;
        self
    }

    /// `h-auto min-h-8 py-0.75`.
    pub fn multiline(mut self, multiline: bool) -> Self {
        self.multiline = multiline;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn tooltip(mut self, tooltip: impl Into<Tooltip>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl Selectable for ThreadDetailsControl {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl ParentElement for ThreadDetailsControl {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for ThreadDetailsControl {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let hover = panel_hover_surface(&theme);
        let interactive = !self.disabled;
        let foreground = match self.tone {
            ThreadDetailsTone::Default => with_alpha(theme.foreground, 0.8).into(),
            ThreadDetailsTone::Muted => with_alpha(theme.muted_foreground, 0.7).into(),
            ThreadDetailsTone::Primary => theme.primary,
            ThreadDetailsTone::Destructive => theme.destructive_foreground,
        };
        let split_member = matches!(
            self.part,
            ThreadDetailsPart::Primary | ThreadDetailsPart::Secondary
        );
        let mut root = div()
            .id(self.id)
            .relative()
            .flex()
            .flex_shrink_0()
            .items_center()
            .whitespace_nowrap()
            .border_1()
            .border_color(gpui_kit::transparent_black())
            .font_weight(FontWeight::MEDIUM)
            .text_color(foreground)
            .when(self.selected && interactive, move |this| this.bg(hover));
        root = match self.part {
            ThreadDetailsPart::Row | ThreadDetailsPart::Select => root
                .h(rems(2.))
                .min_w_0()
                .w_full()
                .justify_start()
                .gap(rems(0.625))
                .px(rems(0.625))
                .rounded(RADIUS_LG)
                .text_size(px(13.)),
            ThreadDetailsPart::Primary => root
                .h(rems(2.))
                .min_w_0()
                .flex_1()
                .justify_start()
                .gap(rems(0.625))
                .px(rems(0.625))
                .rounded_l(RADIUS_LG)
                .text_size(px(13.)),
            ThreadDetailsPart::Secondary => root
                .h(rems(2.))
                .w(rems(2.))
                .justify_center()
                .rounded_r(RADIUS_LG)
                .text_size(px(13.)),
            ThreadDetailsPart::Icon => root
                .size(rems(1.5))
                .justify_center()
                .rounded(RADIUS_MD)
                .p_0(),
            ThreadDetailsPart::Meta => root
                .h(rems(1.5))
                .gap_1()
                .px(rems(0.375))
                .rounded(RADIUS_MD)
                .text_size(TEXT_XS.size)
                // A toned meta control (a row's "Merge" / "Fix") keeps its tone.
                .when(self.tone == ThreadDetailsTone::Default, |this| {
                    this.text_color(theme.muted_foreground)
                }),
        };
        if self.multiline {
            root = root.h_auto().min_h(rems(2.)).py(px(3.));
        }
        if self.part == ThreadDetailsPart::Select {
            root = root.pr_0();
        }
        root = root.children(self.children);
        if interactive {
            root = root.cursor(CursorStyle::PointingHand);
            // The split group's frame owns the hover surface for its halves.
            if !split_member {
                root = root.hover(move |style| style.bg(hover));
            }
            if let Some(handler) = self.on_click {
                root = root.on_click(move |event, window, cx| handler(event, window, cx));
            }
        } else {
            root = root.opacity(0.64);
        }
        if let Some(tooltip) = self.tooltip {
            root = root.tooltip(move |window, cx| tooltip.clone().build(window, cx));
        }
        root
    }
}

/// The select row's trailing chevron cell: `w-8` with a 1px divider.
pub fn panel_select_chevron(theme: &AppTheme) -> AnyElement {
    div()
        .relative()
        .flex()
        .h_full()
        .w(rems(2.))
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .child(
            div()
                .absolute()
                .left(px(-1.))
                .top_0()
                .bottom_0()
                .flex()
                .items_center()
                .child(
                    div()
                        .h(rems(1.))
                        .w(px(1.))
                        .bg(solid_over(theme.border, 0.65, theme.background)),
                ),
        )
        .child(panel_icon(IconName::ChevronDown, theme))
        .into_any_element()
}
