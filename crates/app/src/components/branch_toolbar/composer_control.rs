//! Ports of upstream/apps/web/src/components/chat/ComposerControl.tsx (the
//! `xs` resting-strip look the branch toolbar uses), ComposerContextLabel.tsx
//! and the `ContextStrip` frame of chat/ComposerSurface.tsx.
//!
//! GPUI has no element transforms, so the press scale is not ported; the
//! strip's label collapse animation is replaced by an instant swap (see
//! Deviations).
//!
//! Port of upstream/apps/web/src/components/chat/ComposerControl.tsx.
//! Port of upstream/apps/web/src/components/ComposerContextLabel.tsx.
//! Port of upstream/apps/web/src/components/chat/ComposerSurface.tsx (`ContextStrip` frame).

// A faithful port keeps upstream's whole API (PORTING rule 3); the hosts wire
// the rest as the composer, thread details and settings tasks land.
#![allow(dead_code)]

use std::rc::Rc;

use gpui_kit::component::Selectable;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, CursorStyle, ElementId, InteractiveElement as _, IntoElement,
    ParentElement, RenderOnce, StatefulInteractiveElement as _, Styled, Window, div, px, rems,
};

use crate::theme::color::with_alpha;
use crate::theme::tokens::{self, CONTROL_RADIUS, RADIUS_2XL, TEXT_SM, TEXT_XS, tw};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::icon::{Icon, IconName};
use crate::ui::tooltip::Tooltip;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ComposerControlSize {
    #[default]
    Sm,
    /// The dimmer resting strip.
    Xs,
}

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// `<ComposerControl size>`: `rounded-(--control-radius)`, `h-7 gap-1
/// px-1.75 text-xs font-normal text-muted-foreground/70 hover:bg-accent
/// hover:text-foreground/80` at `xs`.
#[derive(IntoElement)]
pub struct ComposerControl {
    id: ElementId,
    size: ComposerControlSize,
    disabled: bool,
    selected: bool,
    on_click: Option<ClickHandler>,
    tooltip: Option<Tooltip>,
    min_w_0: bool,
    flex_initial: bool,
    children: Vec<AnyElement>,
}

impl ComposerControl {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            size: ComposerControlSize::Sm,
            disabled: false,
            selected: false,
            on_click: None,
            tooltip: None,
            min_w_0: false,
            flex_initial: false,
            children: Vec::new(),
        }
    }

    pub fn size(mut self, size: ComposerControlSize) -> Self {
        self.size = size;
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

    /// `min-w-0 max-w-full`.
    pub fn shrinkable(mut self) -> Self {
        self.min_w_0 = true;
        self
    }

    /// `flex-initial`.
    pub fn flex_initial(mut self) -> Self {
        self.flex_initial = true;
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

impl Selectable for ComposerControl {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl ParentElement for ComposerControl {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for ComposerControl {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let accent = theme.accent;
        let interactive = !self.disabled;
        let xs = self.size == ComposerControlSize::Xs;
        let hover_foreground = if xs {
            with_alpha(theme.foreground, 0.8).into()
        } else {
            theme.foreground
        };
        let mut root = div()
            .id(self.id)
            .relative()
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .whitespace_nowrap()
            .rounded(CONTROL_RADIUS)
            .border_1()
            .border_color(gpui_kit::transparent_black())
            .h(if xs { rems(1.5) } else { rems(1.75) })
            .map(|this| {
                if xs {
                    this.gap_1()
                        .px(rems(0.4375))
                        .text_size(TEXT_XS.size)
                        .text_color(with_alpha(theme.muted_foreground, 0.7))
                } else {
                    this.gap(rems(0.375))
                        .px(rems(0.625))
                        .text_size(TEXT_SM.size)
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(theme.secondary_label)
                }
            })
            .when(self.min_w_0, |this| this.min_w_0().max_w_full())
            .when(self.selected && interactive, move |this| this.bg(accent));
        if interactive {
            root = root
                .cursor(CursorStyle::PointingHand)
                .hover(move |style| style.bg(accent).text_color(hover_foreground));
            if let Some(handler) = self.on_click {
                root = root.on_click(move |event, window, cx| handler(event, window, cx));
            }
        } else {
            root = root.opacity(0.64);
        }
        if let Some(tooltip) = self.tooltip {
            root = root.tooltip(move |window, cx| tooltip.clone().build(window, cx));
        }
        root.children(self.children)
    }
}

/// `ComposerControlChevron` at `xs`: `size-3 opacity-50`, stroke 2.25.
pub fn composer_control_chevron(theme: &AppTheme) -> AnyElement {
    div()
        .flex_shrink_0()
        .opacity(0.5)
        .child(
            Icon::path(crate::assets::CHEVRON_DOWN)
                .size(rems(0.75))
                .color(theme.muted_foreground),
        )
        .into_any_element()
}

/// `ComposerContextLabel` (toolbar): `min-w-0 max-w-[240px]`, collapsing to
/// nothing when the strip is compact.
pub fn composer_context_label(child: impl IntoElement, compact: bool) -> AnyElement {
    div()
        .min_w_0()
        .max_w(px(240.))
        .when(compact, |this| {
            this.max_w(px(0.)).overflow_hidden().opacity(0.)
        })
        .child(div().w_full().min_w_0().truncate().child(child))
        .into_any_element()
}

/// `ComposerSurface.ContextStrip`: the bar hanging below the composer, joined
/// to it by the glass seam: `-mt-4`, `px-2 pt-5 pb-1`, outline `rounded-b-2xl`.
pub fn composer_context_strip(theme: &AppTheme) -> gpui_kit::Div {
    let dark = theme.is_dark();
    let outline = if dark {
        with_alpha(tw::WHITE, 0.07)
    } else {
        with_alpha(tw::BLACK, 0.08)
    };
    div()
        .relative()
        .mx(rems(1.375))
        .mt(px(-16.))
        .flex()
        .items_center()
        .gap_2()
        .overflow_hidden()
        .px_2()
        .pt(rems(1.25))
        .pb_1()
        .rounded_b(RADIUS_2XL)
        .border_1()
        .border_color(outline)
        .when(!dark, |this| this.shadow(tokens::shadow_composer(false)))
        .text_size(TEXT_XS.size)
        .text_color(with_alpha(theme.muted_foreground, 0.7))
}

/// A small helper so call sites read like the upstream icon + label pairs.
pub fn composer_icon(name: IconName, theme: &AppTheme) -> AnyElement {
    Icon::new(name)
        .size(rems(0.75))
        .color(with_alpha(theme.muted_foreground, 0.7).into())
        .into_any_element()
}
