//! Port of upstream/apps/web/src/components/ui/toggle.tsx
//!
//! `toggleVariants` at the desktop (`sm:`) breakpoint: a pressable control
//! with a `pressed` state (`data-pressed`). Base: `rounded-lg border
//! font-medium sm:text-sm text-foreground hover:bg-accent`, icons at
//! `opacity-80` and `sm:size-4`, pressed `bg-input/64
//! text-accent-foreground`, disabled `opacity-64`. As with Button, the base
//! `sm:` text/icon rules win over a size's unprefixed `text-xs` /
//! `size-3.5`, so compact and segmented render 14px text and 16px icons.
//!
//! Not ported: focus ring, coarse-pointer hit-area expansion.

use std::rc::Rc;

use gpui_kit::Refineable as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, CursorStyle, ElementId, FontWeight, Hsla, InteractiveElement,
    IntoElement, MouseButton, ParentElement, Pixels, Rems, RenderOnce, SharedString,
    StatefulInteractiveElement, StyleRefinement, Styled, Window, div, px, rems,
};

use super::icon::{Icon, IconName};
use super::tooltip::Tooltip;
use crate::components::drag_region::no_drag;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{self, RADIUS_LG, RADIUS_MD, TEXT_SM, tw};
use crate::theme::{ActiveAppTheme, AppTheme};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToggleVariant {
    #[default]
    Default,
    Pill,
    Ghost,
    Outline,
    Segmented,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToggleSize {
    Compact,
    #[default]
    Default,
    Lg,
    Segmented,
    Sm,
    Xs,
}

struct SizeSpec {
    height: Rems,
    /// `min-w-*`; `None` for `segmented` (`min-w-0`).
    min_width: Option<Rems>,
    padding_x: Pixels,
    radius: Pixels,
}

impl ToggleSize {
    fn spec(self) -> SizeSpec {
        let spec = |height: f32, padding_x: f32, radius: Pixels| SizeSpec {
            height: rems(height),
            min_width: Some(rems(height)),
            padding_x: px(padding_x),
            radius,
        };
        match self {
            // h-7 min-w-7 rounded-md px-[calc(--spacing(1)-1px)]
            Self::Compact => spec(1.75, 3., RADIUS_MD),
            // sm:h-8 sm:min-w-8 px-[calc(--spacing(2)-1px)]
            Self::Default => spec(2., 7., RADIUS_LG),
            // sm:h-9 sm:min-w-9 px-[calc(--spacing(2.5)-1px)]
            Self::Lg => spec(2.25, 9., RADIUS_LG),
            // h-6 min-w-0 rounded-md px-2.5
            Self::Segmented => SizeSpec {
                min_width: None,
                ..spec(1.5, 10., RADIUS_MD)
            },
            // sm:h-7 sm:min-w-7 px-[calc(--spacing(1.5)-1px)]
            Self::Sm => spec(1.75, 5., RADIUS_LG),
            // sm:h-6 sm:min-w-6 px-[calc(--spacing(1)-1px)] rounded-md
            Self::Xs => spec(1.5, 3., RADIUS_MD),
        }
    }
}

struct VariantColors {
    border: Hsla,
    bg: Option<Hsla>,
    fg: Hsla,
    hover_bg: Option<Hsla>,
    hover_fg: Option<Hsla>,
    pressed_bg: Option<Hsla>,
    pressed_fg: Hsla,
    pill: bool,
    font_normal: bool,
    shadow: bool,
    pressed_shadow: bool,
}

fn a(color: impl Into<gpui_kit::Rgba>, alpha: f32) -> Hsla {
    with_alpha(color, alpha).into()
}

impl ToggleVariant {
    fn colors(self, theme: &AppTheme) -> VariantColors {
        let dark = theme.is_dark();
        let base = VariantColors {
            border: a(tw::BLACK, 0.),
            bg: None,
            fg: theme.foreground,
            hover_bg: Some(theme.accent),
            hover_fg: None,
            // Base `data-pressed:bg-input/64 data-pressed:text-accent-foreground`.
            pressed_bg: Some(a(theme.input, 0.64)),
            pressed_fg: theme.accent_foreground,
            pill: false,
            font_normal: false,
            shadow: false,
            pressed_shadow: false,
        };
        match self {
            Self::Default => base,
            Self::Pill => VariantColors {
                fg: theme.muted_foreground,
                pressed_fg: theme.foreground,
                pill: true,
                font_normal: true,
                ..base
            },
            Self::Ghost => VariantColors {
                pressed_bg: Some(theme.accent),
                ..base
            },
            Self::Outline => VariantColors {
                border: theme.input,
                bg: Some(if dark {
                    a(theme.input, 0.32)
                } else {
                    theme.background
                }),
                hover_bg: Some(if dark {
                    a(theme.input, 0.64)
                } else {
                    theme.accent
                }),
                pressed_bg: Some(if dark {
                    theme.input
                } else {
                    a(theme.input, 0.64)
                }),
                shadow: true,
                ..base
            },
            Self::Segmented => VariantColors {
                fg: theme.muted_foreground,
                hover_bg: Some(if dark {
                    a(theme.input, 0.32)
                } else {
                    a(theme.background, 0.55)
                }),
                hover_fg: Some(theme.foreground),
                pressed_bg: Some(if dark {
                    a(theme.input, 0.72)
                } else {
                    theme.background
                }),
                pressed_fg: theme.foreground,
                pressed_shadow: true,
                ..base
            },
        }
    }
}

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// `<Toggle variant size pressed>`.
#[derive(IntoElement)]
pub struct Toggle {
    id: ElementId,
    variant: ToggleVariant,
    size: ToggleSize,
    icon: Option<IconName>,
    icon_size: Option<Rems>,
    label: Option<SharedString>,
    children: Vec<AnyElement>,
    pressed: bool,
    disabled: bool,
    tooltip: Option<Tooltip>,
    on_click: Option<ClickHandler>,
    style: StyleRefinement,
}

impl Toggle {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            variant: ToggleVariant::Default,
            size: ToggleSize::Default,
            icon: None,
            icon_size: None,
            label: None,
            children: Vec::new(),
            pressed: false,
            disabled: false,
            tooltip: None,
            on_click: None,
            style: StyleRefinement::default(),
        }
    }

    pub fn variant(mut self, variant: ToggleVariant) -> Self {
        self.variant = variant;
        self
    }

    pub fn size(mut self, size: ToggleSize) -> Self {
        self.size = size;
        self
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    /// An explicit `size-*` on the icon (default `sm:size-4`).
    pub fn icon_size(mut self, size: Rems) -> Self {
        self.icon_size = Some(size);
        self
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn pressed(mut self, pressed: bool) -> Self {
        self.pressed = pressed;
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

    /// Fires on press; the owner flips `pressed` (upstream `onPressedChange`).
    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl Styled for Toggle {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl ParentElement for Toggle {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for Toggle {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let dark = theme.is_dark();
        let spec = self.size.spec();
        let colors = self.variant.colors(theme);
        let interactive = !self.disabled;
        let pressed = self.pressed;
        let ghost_disabled = self.disabled && self.variant == ToggleVariant::Ghost;

        let fg = if ghost_disabled {
            // ghost: `disabled:opacity-100 disabled:text-muted-foreground`.
            theme.muted_foreground
        } else if pressed {
            colors.pressed_fg
        } else {
            colors.fg
        };
        let bg = if pressed {
            colors.pressed_bg.or(colors.bg)
        } else {
            colors.bg
        };
        // `[&_svg:not([class*='opacity-'])]:opacity-80`; ghost disabled resets it.
        let icon_color = if ghost_disabled { fg } else { a(fg, 0.8) };
        let radius = if colors.pill { px(9999.) } else { spec.radius };
        let padding_x = if colors.pill { px(12.) } else { spec.padding_x };

        let mut root = div()
            .id(self.id.clone())
            .on_mouse_down(MouseButton::Left, no_drag)
            .relative()
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .gap_2()
            .whitespace_nowrap()
            .h(spec.height)
            .when_some(spec.min_width, |this, width| this.min_w(width))
            .px(padding_x)
            .rounded(radius)
            .border_1()
            .border_color(colors.border)
            .font_weight(if colors.font_normal {
                FontWeight::NORMAL
            } else {
                FontWeight::MEDIUM
            })
            .text_size(TEXT_SM.size)
            .line_height(TEXT_SM.line_height)
            .text_color(fg)
            .when_some(bg, |this, bg| this.bg(bg))
            .when(colors.shadow && !pressed && interactive, |this| {
                this.shadow(tokens::shadow_xs(0.05))
            })
            .when(colors.pressed_shadow && pressed, |this| {
                this.shadow(tokens::shadow_xs(0.10))
            })
            .when(self.disabled && !ghost_disabled, |this| this.opacity(0.64))
            .when(interactive, |this| {
                this.cursor(CursorStyle::PointingHand).hover(|style| {
                    let mut style = style;
                    if !pressed {
                        if let Some(bg) = colors.hover_bg {
                            style = style.bg(bg);
                        }
                        if let Some(fg) = colors.hover_fg {
                            style = style.text_color(fg);
                        }
                    }
                    style
                })
            });

        if let Some(icon) = self.icon {
            root = root.child(
                Icon::new(icon)
                    .size(self.icon_size.unwrap_or(rems(1.)))
                    .color(icon_color),
            );
        }
        if let Some(label) = self.label {
            root = root.child(label);
        }
        root = root.children(self.children);
        // Outline: `not-disabled:not-active:not-data-pressed:before:shadow-[…]`.
        if colors.shadow && interactive && !pressed {
            root = root.child(tokens::control_lip_overlay(
                tokens::control_lip(dark),
                radius,
            ));
        }
        if let Some(on_click) = self.on_click.filter(|_| interactive) {
            root = root.on_click(move |event, window, cx| on_click(event, window, cx));
        }
        if let Some(tooltip) = self.tooltip {
            root = root.tooltip(move |window, cx| tooltip.clone().build(window, cx));
        }
        root.style().refine(&self.style);
        root
    }
}
