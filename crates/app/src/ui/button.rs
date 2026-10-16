//! Port of upstream/apps/web/src/components/ui/button.tsx
//!
//! Upstream's `buttonVariants` cva table, translated to GPUI styles at the
//! desktop (`sm:`) breakpoint. Variant and size names match upstream.
//! Built on GPUI primitives rather than gpui-component's `Button`, whose
//! fixed size/padding/text ladder cannot express upstream's table; tooltips
//! and icons come from gpui-component.
//!
//! Not ported: `media-close` / `media-navigation` (preview/media viewers are
//! cut), the `render` prop (wrap children instead), `active:scale-[0.97]`
//! (GPUI has no element transforms) and coarse-pointer hit-area expansion.

use std::rc::Rc;

use gpui_kit::Refineable as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, BoxShadow, ClickEvent, CursorStyle, ElementId, FontWeight, Hsla,
    InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels, Rems, RenderOnce,
    SharedString, StatefulInteractiveElement, StyleRefinement, Styled, Window, div, px,
};

use super::icon::{Icon, IconName};
use super::tooltip::Tooltip;
use crate::components::drag_region::no_drag;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{self, RADIUS_MD, RADIUS_SM, TEXT_BASE, TEXT_SM, TEXT_XS, TextSize, tw};
use crate::theme::{ActiveAppTheme, AppTheme};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonVariant {
    #[default]
    Default,
    Destructive,
    DestructiveOutline,
    Ghost,
    GhostMuted,
    GhostDestructive,
    Glass,
    Link,
    Outline,
    Overlay,
    Secondary,
    WarningOutline,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonSize {
    Compact,
    #[default]
    Default,
    Icon,
    IconLg,
    IconMicro,
    IconTiny,
    IconSm,
    IconXl,
    IconXs,
    Lg,
    Micro,
    Sm,
    SmMultiline,
    Xl,
    Xs,
}

/// One row of upstream's `size` table, desktop values.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SizeSpec {
    /// Fixed height; `None` for `sm-multiline` (min height instead).
    pub height: Option<Rems>,
    pub min_height: Option<Rems>,
    /// Square icon sizes set the width too.
    pub square: bool,
    /// `px-[calc(--spacing(n)-1px)]`: the 1px is the border.
    pub padding_x: Pixels,
    pub padding_y: Pixels,
    pub gap: Rems,
    pub text: TextSize,
    pub radius: Pixels,
    pub icon: Rems,
}

const fn spec(height: f32, padding_x: f32, gap: f32, text: TextSize, icon: f32) -> SizeSpec {
    SizeSpec {
        height: Some(Rems(height)),
        min_height: None,
        square: false,
        padding_x: px(padding_x),
        padding_y: px(0.),
        gap: Rems(gap),
        text,
        radius: px(8.),
        icon: Rems(icon),
    }
}

const fn square(size: f32, icon: f32) -> SizeSpec {
    SizeSpec {
        square: true,
        padding_x: px(0.),
        ..spec(size, 0., 0.5, TEXT_SM, icon)
    }
}

impl ButtonSize {
    pub(crate) fn spec(self) -> SizeSpec {
        // Desktop cascade: the base class's `sm:text-sm` and
        // `sm:[&_svg]:size-4` come after a size's unprefixed `text-xs` /
        // `[&_svg]:size-3.5` in the compiled CSS, so on desktop they win.
        // Sizes that add their own `sm:` overrides (micro, xs, xl) keep them.
        match self {
            // h-7 gap-1 rounded-md px-[calc(--spacing(2)-1px)]; text-xs and
            // svg size-3.5 lose to the base sm: rules → 14px text, 16px icon.
            Self::Compact => SizeSpec {
                radius: RADIUS_MD,
                ..spec(1.75, 7., 0.25, TEXT_SM, 1.)
            },
            // sm:h-8 px-[calc(--spacing(3)-1px)], sm:text-sm, svg sm:size-4
            Self::Default => spec(2., 11., 0.5, TEXT_SM, 1.),
            Self::Icon => square(2., 1.),
            Self::IconLg => square(2.25, 1.),
            // size-5 rounded-sm p-0, svg size-3. The cascade would make the
            // default icon 16px, but nearly every upstream icon-micro /
            // icon-tiny call site passes an explicit `size-3`, so 12px is the
            // default here; use `Button::icon_size` for the rest.
            Self::IconMicro => SizeSpec {
                radius: RADIUS_SM,
                ..square(1.25, 0.75)
            },
            Self::IconTiny => square(1., 0.75),
            Self::IconSm => square(1.75, 1.),
            Self::IconXl => square(2.5, 1.125),
            Self::IconXs => square(1.5, 0.875),
            Self::Lg => spec(2.25, 13., 0.5, TEXT_SM, 1.),
            // h-5 gap-1 rounded-sm px-[calc(--spacing(1.5)-1px)] text-[11px], svg size-3
            Self::Micro => SizeSpec {
                radius: RADIUS_SM,
                ..spec(1.25, 5., 0.25, tokens::TEXT_2XS, 0.75)
            },
            Self::Sm => spec(1.75, 9., 0.375, TEXT_SM, 1.),
            Self::SmMultiline => SizeSpec {
                height: None,
                min_height: Some(Rems(1.75)),
                padding_y: px(3.),
                ..spec(1.75, 9., 0.375, TEXT_SM, 1.)
            },
            Self::Xl => spec(2.5, 15., 0.5, TEXT_BASE, 1.125),
            Self::Xs => spec(1.5, 7., 0.25, TEXT_XS, 0.875),
        }
    }
}

/// Resolved colors for one variant in one state.
#[derive(Clone, Debug)]
pub(crate) struct VariantColors {
    pub bg: Option<Hsla>,
    pub border: Hsla,
    pub fg: Hsla,
    /// `--control-icon-color`; `None` = currentColor.
    pub icon: Option<Hsla>,
    pub bg_hover: Option<Hsla>,
    pub border_hover: Option<Hsla>,
    pub fg_hover: Option<Hsla>,
    pub bg_active: Option<Hsla>,
    pub shadow: Vec<BoxShadow>,
    pub underline_on_hover: bool,
    pub pill: bool,
    /// Outline controls: the 1px lip over the bottom (light) / top (dark) border.
    pub lip: bool,
}

fn a(color: impl Into<gpui_kit::Rgba>, alpha: f32) -> Hsla {
    with_alpha(color, alpha).into()
}

impl ButtonVariant {
    pub(crate) fn colors(self, theme: &AppTheme) -> VariantColors {
        let dark = theme.is_dark();
        let transparent = a(tw::BLACK, 0.);
        let base = VariantColors {
            bg: None,
            border: transparent,
            fg: theme.foreground,
            icon: None,
            bg_hover: None,
            border_hover: None,
            fg_hover: None,
            bg_active: None,
            shadow: Vec::new(),
            underline_on_hover: false,
            pill: false,
            lip: false,
        };
        // Solid fills: inset top highlight plus a tinted shadow-xs.
        let solid_shadow = |tint: Hsla| {
            let mut drop = tokens::shadow_xs(1.);
            drop[0].color = a(tint, 0.24);
            drop.push(tokens::inset_control_highlight());
            drop
        };
        // Outline fills: bg-popover (dark bg-input/32), shadow-xs/5 and a 1px lip.
        let outline_bg = if dark {
            a(theme.input, 0.32)
        } else {
            theme.popover
        };
        let outline_shadow = || tokens::shadow_xs(0.05);
        match self {
            Self::Default => VariantColors {
                bg: Some(theme.primary),
                border: theme.primary,
                fg: theme.primary_foreground,
                bg_hover: Some(a(theme.primary, 0.9)),
                shadow: solid_shadow(theme.primary),
                ..base
            },
            Self::Destructive => VariantColors {
                bg: Some(theme.destructive),
                border: theme.destructive,
                fg: tw::WHITE.into(),
                bg_hover: Some(a(theme.destructive, 0.9)),
                shadow: solid_shadow(theme.destructive),
                ..base
            },
            Self::DestructiveOutline => VariantColors {
                bg: Some(outline_bg),
                border: theme.input,
                fg: theme.destructive_foreground,
                bg_hover: Some(a(theme.destructive, 0.04)),
                border_hover: Some(a(theme.destructive, 0.32)),
                shadow: outline_shadow(),
                lip: true,
                ..base
            },
            Self::Ghost => VariantColors {
                icon: Some(theme.muted_foreground),
                bg_hover: Some(theme.accent),
                ..base
            },
            Self::GhostMuted => VariantColors {
                fg: theme.muted_foreground,
                bg_hover: Some(theme.accent),
                fg_hover: Some(theme.foreground),
                ..base
            },
            Self::GhostDestructive => VariantColors {
                fg: theme.muted_foreground,
                bg_hover: Some(theme.accent),
                fg_hover: Some(theme.destructive),
                ..base
            },
            Self::Glass => VariantColors {
                bg: Some(a(theme.background, 0.8)),
                border: a(theme.border, 0.6),
                icon: Some(theme.muted_foreground),
                border_hover: Some(theme.border),
                shadow: tokens::shadow_sm(0.1),
                pill: true,
                ..base
            },
            Self::Link => VariantColors {
                underline_on_hover: true,
                ..base
            },
            Self::Outline => VariantColors {
                bg: Some(outline_bg),
                border: theme.input,
                icon: Some(theme.muted_foreground),
                bg_hover: Some(if dark {
                    a(theme.input, 0.64)
                } else {
                    a(theme.accent, 0.5)
                }),
                shadow: outline_shadow(),
                lip: true,
                ..base
            },
            Self::Overlay => VariantColors {
                bg: Some(a(tw::BLACK, 0.7)),
                fg: a(tw::WHITE, 0.65),
                bg_hover: Some(a(tw::BLACK, 0.9)),
                ..base
            },
            Self::Secondary => VariantColors {
                bg: Some(theme.secondary),
                fg: theme.secondary_foreground,
                bg_hover: Some(a(theme.secondary, 0.9)),
                bg_active: Some(a(theme.secondary, 0.8)),
                ..base
            },
            Self::WarningOutline => VariantColors {
                bg: Some(theme.warning_surface),
                border: a(theme.warning, 0.32),
                fg: theme.warning_foreground,
                bg_hover: Some(a(theme.warning, if dark { 0.24 } else { 0.16 })),
                border_hover: Some(a(theme.warning, 0.4)),
                shadow: tokens::shadow_xs(0.05),
                ..base
            },
        }
    }
}

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// `<Button variant size>`.
#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    variant: ButtonVariant,
    size: ButtonSize,
    icon: Option<IconName>,
    icon_size: Option<Rems>,
    label: Option<SharedString>,
    children: Vec<AnyElement>,
    disabled: bool,
    pressed: bool,
    tooltip: Option<Tooltip>,
    on_click: Option<ClickHandler>,
    color_overrides: ColorOverrides,
    style: StyleRefinement,
}

/// `className` colour overrides on top of the variant (`text-*`,
/// `hover:bg-*`, `hover:text-*`).
#[derive(Clone, Copy, Debug, Default)]
struct ColorOverrides {
    fg: Option<Hsla>,
    bg_hover: Option<Hsla>,
    fg_hover: Option<Hsla>,
}

impl Button {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            variant: ButtonVariant::Default,
            size: ButtonSize::Default,
            icon: None,
            icon_size: None,
            label: None,
            children: Vec::new(),
            disabled: false,
            pressed: false,
            tooltip: None,
            on_click: None,
            color_overrides: ColorOverrides::default(),
            style: StyleRefinement::default(),
        }
    }

    /// A `text-*` class on top of the variant's foreground.
    pub fn fg(mut self, color: impl Into<Hsla>) -> Self {
        self.color_overrides.fg = Some(color.into());
        self
    }

    /// A `hover:bg-*` class on top of the variant's hover fill.
    pub fn hover_bg(mut self, color: impl Into<Hsla>) -> Self {
        self.color_overrides.bg_hover = Some(color.into());
        self
    }

    /// A `hover:text-*` class on top of the variant's hover foreground.
    pub fn hover_fg(mut self, color: impl Into<Hsla>) -> Self {
        self.color_overrides.fg_hover = Some(color.into());
        self
    }

    pub fn variant(mut self, variant: ButtonVariant) -> Self {
        self.variant = variant;
        self
    }

    pub fn size(mut self, size: ButtonSize) -> Self {
        self.size = size;
        self
    }

    /// Leading icon, sized and tinted by the variant (`[&_svg]` rules).
    /// An explicit `size-*` on the icon, overriding the size's default.
    pub fn icon_size(mut self, size: Rems) -> Self {
        self.icon_size = Some(size);
        self
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// `data-pressed`: paints the hover state permanently (toggles, open menus).
    pub fn pressed(mut self, pressed: bool) -> Self {
        self.pressed = pressed;
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

/// Lets a [`Button`] trigger gpui-component popovers: while the popup is open the trigger is
/// marked `selected`, which here is upstream's `data-pressed`.
impl gpui_kit::component::Selectable for Button {
    fn selected(self, selected: bool) -> Self {
        self.pressed(selected)
    }

    fn is_selected(&self) -> bool {
        self.pressed
    }
}

impl Styled for Button {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl ParentElement for Button {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for Button {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let size = self.size.spec();
        let mut colors = self.variant.colors(theme);
        if let Some(fg) = self.color_overrides.fg {
            colors.fg = fg;
        }
        if let Some(bg) = self.color_overrides.bg_hover {
            colors.bg_hover = Some(bg);
        }
        if let Some(fg) = self.color_overrides.fg_hover {
            colors.fg_hover = Some(fg);
        }
        let group: SharedString = format!("button-{:?}", self.id).into();
        let interactive = !self.disabled;
        let pressed = self.pressed && interactive;

        let bg = if pressed {
            colors.bg_hover.or(colors.bg)
        } else {
            colors.bg
        };
        let fg = if pressed {
            colors.fg_hover.unwrap_or(colors.fg)
        } else {
            colors.fg
        };
        // `rounded-[var(--control-radius)]`, or the size's own radius;
        // `glass` is a pill.
        let radius = if colors.pill { px(9999.) } else { size.radius };
        let icon_color = colors.icon;
        let icon_hover = colors.fg_hover.filter(|_| icon_color.is_none());
        let theme_fg_for_icon = icon_color.unwrap_or(fg);

        let mut root = div()
            .id(self.id.clone())
            // `no-drag`: buttons inside titlebar drag regions keep their clicks.
            .on_mouse_down(MouseButton::Left, no_drag)
            .group(group.clone())
            .relative()
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .gap(size.gap)
            .whitespace_nowrap()
            .border_1()
            .border_color(colors.border)
            .rounded(radius)
            .font_weight(FontWeight::MEDIUM)
            .text_size(size.text.size)
            .line_height(size.text.line_height)
            .text_color(fg)
            .when_some(size.height, |this, height| this.h(height))
            .when_some(size.min_height, |this, height| this.min_h(height))
            .when(size.square, |this| {
                this.when_some(size.height, |this, height| this.w(height))
            })
            .px(size.padding_x)
            .py(size.padding_y)
            .when(matches!(self.size, ButtonSize::SmMultiline), |this| {
                this.whitespace_normal()
            })
            .when_some(bg, |this, bg| this.bg(bg))
            .when(
                !colors.shadow.is_empty() && !pressed && interactive,
                |this| this.shadow(colors.shadow.clone()),
            )
            .when(!interactive, |this| this.opacity(0.64))
            .when(interactive, |this| {
                this.cursor(CursorStyle::PointingHand)
                    .hover(|style| {
                        let mut style = style;
                        if let Some(bg) = colors.bg_hover {
                            style = style.bg(bg);
                        }
                        if let Some(border) = colors.border_hover {
                            style = style.border_color(border);
                        }
                        if let Some(fg) = colors.fg_hover {
                            style = style.text_color(fg);
                        }
                        if colors.underline_on_hover {
                            style = style.underline();
                        }
                        style
                    })
                    .when_some(colors.bg_active, |this, bg| {
                        this.active(|style| style.bg(bg))
                    })
            });

        if let Some(icon) = self.icon {
            root = root.child(
                Icon::new(icon)
                    .size(self.icon_size.unwrap_or(size.icon))
                    .color(theme_fg_for_icon)
                    .when_some(icon_hover.filter(|_| interactive), |this, hover| {
                        this.group_hover_color(group.clone(), hover)
                    }),
            );
        }
        if let Some(label) = self.label {
            root = root.child(label);
        }
        root = root.children(self.children);
        // `not-disabled:not-active:not-data-pressed:before:shadow-[…]`.
        if colors.lip && interactive && !pressed {
            root = root.child(tokens::control_lip_overlay(
                tokens::control_lip(theme.is_dark()),
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
