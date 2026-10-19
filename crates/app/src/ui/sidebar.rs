//! Port of upstream/apps/web/src/components/ui/sidebar.tsx (desktop parts).
//!
//! Upstream keeps the open state in `SidebarProvider` context and reads it
//! with `useSidebar()`. GPUI has no context, so the owner (the app layout)
//! holds the state and passes `open` to [`Sidebar`] and [`SidebarTrigger`];
//! the components keep upstream's names, geometry and tokens. Mobile
//! (`Sheet`, `openMobile`, `ui/sidebarState.ts`), the cookie, the `icon` and
//! `floating`/`inset` variants and `SidebarMenuSub*` are not ported (mobile
//! is cut; no desktop caller uses the rest).

use gpui_kit::component::input::{Input, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, CursorStyle, Div, ElementId, Entity, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, ParentElement, Pixels,
    RenderOnce, SharedString, StatefulInteractiveElement as _, Styled, Window, div, px, rems,
};

use super::{Button, ButtonSize, ButtonVariant, Icon, IconName, Tooltip, scroll_area_hidden};
use crate::components::drag_region::no_drag;
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{
    CONTROL_RADIUS, SIDEBAR_CONTENT_INSET, SIDEBAR_ROW_CONTENT_INSET, TEXT_SM, TEXT_XS,
};

/// `SIDEBAR_WIDTH` (16rem); the thread sidebar overrides it with its own width.
pub const SIDEBAR_WIDTH: f32 = 256.;

pub type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;
type MouseDownHandler = Box<dyn Fn(&MouseDownEvent, &mut Window, &mut App) + 'static>;

/// Parent classes win over the component's own, like `twMerge`.
fn apply_overrides(base: Div, overrides: Vec<Box<dyn FnOnce(Div) -> Div>>) -> Div {
    overrides.into_iter().fold(base, |base, apply| apply(base))
}

/// A plain container with upstream's classes and children.
macro_rules! container {
    ($(#[$meta:meta])* $name:ident, $style:expr) => {
        $(#[$meta])*
        #[derive(IntoElement, Default)]
        pub struct $name {
            children: Vec<AnyElement>,
            overrides: Vec<Box<dyn FnOnce(Div) -> Div>>,
        }

        impl $name {
            pub fn new() -> Self {
                Self::default()
            }

            /// Layout classes from the parent (`className` on the upstream call
            /// site). They apply after the component's own classes, so they
            /// win like `twMerge` does (a `flex-row` replaces `flex-col`).
            pub fn with(mut self, f: impl FnOnce(Div) -> Div + 'static) -> Self {
                self.overrides.push(Box::new(f));
                self
            }
        }

        impl ParentElement for $name {
            fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
                self.children.extend(elements);
            }
        }

        impl RenderOnce for $name {
            fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
                let style: fn(Div, &App) -> Div = $style;
                apply_overrides(style(div(), cx), self.overrides).children(self.children)
            }
        }
    };
}

container!(
    /// `SidebarProvider`: `group/sidebar-wrapper flex min-h-svh w-full`.
    SidebarProvider,
    |this, _| this.relative().flex().flex_row().size_full()
);

container!(
    /// `SidebarInset`: `relative flex min-w-0 w-full flex-1 flex-col bg-background`.
    SidebarInset,
    |this, cx| {
        this.relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .w_full()
            .bg(cx.app_theme().background)
    }
);

container!(
    /// `SidebarHeader`: `flex flex-col gap-2 p-2`.
    SidebarHeader,
    |this, _| this.flex().flex_col().gap_2().p_2()
);

container!(
    /// `SidebarFooter`: `flex flex-col gap-2 px-(--sidebar-content-inset) py-1`.
    SidebarFooter,
    |this, _| this.flex().flex_col().gap_2().px(SIDEBAR_CONTENT_INSET).py_1()
);

container!(
    /// `SidebarGroup`: `relative flex w-full min-w-0 flex-col p-(--sidebar-content-inset)`.
    SidebarGroup,
    |this, _| {
        this.relative()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .p(SIDEBAR_CONTENT_INSET)
    }
);

container!(
    /// `SidebarMenu`: `flex w-full min-w-0 flex-col gap-1`.
    SidebarMenu,
    |this, _| this.flex().flex_col().w_full().min_w_0().gap_1()
);

container!(
    /// `SidebarMenuItem`: `group/menu-item relative`.
    SidebarMenuItem,
    |this, _| this.relative()
);

/// `Sidebar side="left" collapsible="offcanvas"` (flush variant): the
/// `bg-sidebar` column with its right border. Offcanvas collapse removes it.
#[derive(IntoElement)]
pub struct Sidebar {
    open: bool,
    width: Pixels,
    children: Vec<AnyElement>,
}

impl Sidebar {
    pub fn new(open: bool) -> Self {
        Self {
            open,
            width: px(SIDEBAR_WIDTH),
            children: Vec::new(),
        }
    }

    /// `--sidebar-width`.
    pub fn width(mut self, width: Pixels) -> Self {
        self.width = width;
        self
    }
}

impl ParentElement for Sidebar {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for Sidebar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let scope = theme.sidebar_scope();
        div().when(self.open, |this| {
            this.relative()
                .flex()
                .flex_col()
                .flex_shrink_0()
                .h_full()
                .w(self.width)
                .bg(scope.sidebar)
                .border_r_1()
                .border_color(scope.container_border)
                .text_color(scope.sidebar_foreground)
                .children(self.children)
        })
    }
}

/// `SidebarTrigger`: ghost icon button toggling the sidebar
/// (`size-[--workspace-titlebar-control-size]`, `no-drag`).
#[derive(IntoElement)]
pub struct SidebarTrigger {
    open: bool,
    tooltip: Tooltip,
    on_click: Option<ClickHandler>,
}

impl SidebarTrigger {
    pub fn new(open: bool) -> Self {
        Self {
            open,
            tooltip: Tooltip::new("Toggle Sidebar"),
            on_click: None,
        }
    }

    pub fn tooltip(mut self, tooltip: impl Into<Tooltip>) -> Self {
        self.tooltip = tooltip.into();
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

impl RenderOnce for SidebarTrigger {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let handler = self.on_click;
        Button::new("sidebar-trigger")
            .variant(ButtonVariant::Ghost)
            .size(ButtonSize::IconSm)
            .icon(if self.open {
                IconName::PanelLeftClose
            } else {
                IconName::PanelLeft
            })
            .pressed(self.open)
            .tooltip(self.tooltip)
            .on_click(move |event, window, cx| {
                if let Some(handler) = &handler {
                    handler(event, window, cx);
                }
            })
    }
}

/// `SidebarRail`: a 16px hit area centred on the sidebar edge that shows a
/// 2px `sidebar-border` line on hover. The owner runs the resize drag
/// (upstream `useResizeDrag`) from `on_mouse_down`.
#[derive(IntoElement)]
pub struct SidebarRail {
    left: Pixels,
    active: bool,
    can_resize: bool,
    on_mouse_down: Option<MouseDownHandler>,
    on_click: Option<ClickHandler>,
}

impl SidebarRail {
    /// `left` is the sidebar's right edge.
    pub fn new(left: Pixels) -> Self {
        Self {
            left,
            active: false,
            can_resize: true,
            on_mouse_down: None,
            on_click: None,
        }
    }

    /// A resize drag is in progress.
    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    pub fn can_resize(mut self, can_resize: bool) -> Self {
        self.can_resize = can_resize;
        self
    }

    pub fn on_mouse_down(
        mut self,
        handler: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_mouse_down = Some(Box::new(handler));
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

impl RenderOnce for SidebarRail {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let border = cx.app_theme().sidebar_scope().sidebar_border;
        let title = if self.can_resize {
            "Drag to resize sidebar"
        } else {
            "Toggle Sidebar"
        };
        div()
            .id("sidebar-rail")
            .group("sidebar-rail")
            .absolute()
            .top_0()
            .bottom_0()
            .left(self.left - px(8.))
            .w(px(16.))
            .cursor(if self.can_resize {
                CursorStyle::ResizeLeftRight
            } else {
                CursorStyle::PointingHand
            })
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(7.))
                    .w(px(2.))
                    .when(self.active, |this| this.bg(border))
                    .group_hover("sidebar-rail", |style| style.bg(border)),
            )
            .when_some(self.on_mouse_down, |this, handler| {
                this.on_mouse_down(MouseButton::Left, move |event, window, cx| {
                    handler(event, window, cx);
                })
            })
            .when_some(self.on_click, |this, handler| {
                this.on_click(move |event, window, cx| handler(event, window, cx))
            })
            .tooltip(move |window, cx| Tooltip::new(title).build(window, cx))
    }
}

/// `SidebarContent`: an optional fixed header above a scroll area with
/// hidden scrollbars (`scrollFade` is not ported; see Deviations).
#[derive(IntoElement)]
pub struct SidebarContent {
    id: ElementId,
    fixed_header: Option<AnyElement>,
    children: Vec<AnyElement>,
}

impl SidebarContent {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            fixed_header: None,
            children: Vec::new(),
        }
    }

    pub fn fixed_header(mut self, header: impl IntoElement) -> Self {
        self.fixed_header = Some(header.into_any_element());
        self
    }
}

impl ParentElement for SidebarContent {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for SidebarContent {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .w_full()
            .when_some(self.fixed_header, |this, header| {
                this.child(div().w_full().flex_shrink_0().child(header))
            })
            .child(
                div().flex_1().min_h_0().child(
                    scroll_area_hidden(self.id).child(
                        div()
                            .flex()
                            .flex_col()
                            .w_full()
                            .min_w_0()
                            .min_h_full()
                            .children(self.children),
                    ),
                ),
            )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SidebarMenuButtonSize {
    /// `h-8 rounded-(--control-radius) px-(--sidebar-row-content-inset) py-1.5 text-sm`.
    #[default]
    Default,
    /// `size-8 justify-center rounded-(--control-radius) p-0`.
    Icon,
    /// `h-12 rounded-lg p-2 text-sm`.
    Lg,
    /// `h-7 rounded-lg p-2 text-xs`.
    Sm,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SidebarMenuButtonVariant {
    /// `font-medium text-sidebar-muted-foreground/80`.
    #[default]
    Default,
    /// `bg-sidebar-control-surface ring-1 ring-sidebar-border`.
    Outline,
}

/// `SidebarMenuButton` (`sidebarMenuButtonVariants`).
#[derive(IntoElement)]
pub struct SidebarMenuButton {
    id: ElementId,
    size: SidebarMenuButtonSize,
    variant: SidebarMenuButtonVariant,
    /// Overrides the size's box (`className="size-7"` on header buttons).
    box_size: Option<gpui_kit::Rems>,
    icon: Option<IconName>,
    leading: Option<AnyElement>,
    label: Option<SharedString>,
    is_active: bool,
    disabled: bool,
    tooltip: Option<Tooltip>,
    on_click: Option<ClickHandler>,
}

impl SidebarMenuButton {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            size: SidebarMenuButtonSize::Default,
            variant: SidebarMenuButtonVariant::Default,
            box_size: None,
            icon: None,
            leading: None,
            label: None,
            is_active: false,
            disabled: false,
            tooltip: None,
            on_click: None,
        }
    }

    pub fn size(mut self, size: SidebarMenuButtonSize) -> Self {
        self.size = size;
        self
    }

    pub fn variant(mut self, variant: SidebarMenuButtonVariant) -> Self {
        self.variant = variant;
        self
    }

    pub fn box_size(mut self, size: gpui_kit::Rems) -> Self {
        self.box_size = Some(size);
        self
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    /// A custom leading element (e.g. a project favicon instead of an icon).
    pub fn leading(mut self, element: impl IntoElement) -> Self {
        self.leading = Some(element.into_any_element());
        self
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// `data-active`.
    pub fn active(mut self, active: bool) -> Self {
        self.is_active = active;
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
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for SidebarMenuButton {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let scope = theme.sidebar_scope();
        let group: SharedString = format!("sidebar-menu-button-{:?}", self.id).into();
        let icon_color: Hsla = if self.is_active {
            scope.sidebar_foreground
        } else {
            theme.sidebar_icon_color
        };
        let text: Hsla = match (self.is_active, self.variant) {
            (true, _) => scope.sidebar_foreground,
            (false, SidebarMenuButtonVariant::Default) => {
                crate::theme::color::with_alpha(scope.sidebar_muted_foreground, 0.8).into()
            }
            (false, SidebarMenuButtonVariant::Outline) => scope.sidebar_foreground,
        };
        let disabled = self.disabled;
        let hover_fg = scope.sidebar_foreground;
        let (hover_bg, active_bg) = (scope.sidebar_row_hover, scope.sidebar_row_active);
        div()
            .id(self.id)
            .group(group.clone())
            .on_mouse_down(MouseButton::Left, no_drag)
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(rems(0.5))
            .overflow_hidden()
            .text_color(text)
            .map(|this| match self.size {
                SidebarMenuButtonSize::Default => this
                    .h_8()
                    .w_full()
                    .rounded(CONTROL_RADIUS)
                    .px(SIDEBAR_ROW_CONTENT_INSET)
                    .py(rems(0.375))
                    .text_size(TEXT_SM.size),
                SidebarMenuButtonSize::Icon => {
                    this.size_8().justify_center().rounded(CONTROL_RADIUS)
                }
                SidebarMenuButtonSize::Lg => this
                    .h_12()
                    .w_full()
                    .rounded_lg()
                    .p_2()
                    .text_size(TEXT_SM.size),
                SidebarMenuButtonSize::Sm => this
                    .h_7()
                    .w_full()
                    .rounded_lg()
                    .p_2()
                    .text_size(TEXT_XS.size),
            })
            .when_some(self.box_size, |this, size| this.size(size))
            .when(self.variant == SidebarMenuButtonVariant::Default, |this| {
                this.font_weight(FontWeight::MEDIUM)
            })
            .when(self.variant == SidebarMenuButtonVariant::Outline, |this| {
                this.bg(scope.sidebar_control_surface)
                    .border_1()
                    .border_color(scope.sidebar_border)
            })
            .when(self.is_active, |this| {
                this.bg(scope.sidebar_row_selected)
                    .font_weight(FontWeight::MEDIUM)
            })
            .map(|this| {
                if disabled {
                    this.opacity(0.64)
                } else {
                    this.cursor(CursorStyle::PointingHand)
                        .hover(move |style| style.bg(hover_bg).text_color(hover_fg))
                        .active(move |style| style.bg(active_bg).text_color(hover_fg))
                }
            })
            .when_some(self.leading, |this, leading| this.child(leading))
            .when_some(self.icon, |this, icon| {
                this.child(
                    Icon::new(icon)
                        .color(icon_color)
                        .group_hover_color(group.clone(), hover_fg),
                )
            })
            .when_some(self.label, |this, label| {
                this.child(div().min_w_0().truncate().child(label))
            })
            .when_some(self.tooltip, |this, tooltip| {
                this.tooltip(move |window, cx| tooltip.clone().build(window, cx))
            })
            .when_some(self.on_click.filter(|_| !disabled), |this, handler| {
                this.on_click(move |event, window, cx| handler(event, window, cx))
            })
    }
}

/// `SidebarInput`: an unstyled input in `text-sm font-medium` sidebar
/// foreground with a `sidebar-muted-foreground` placeholder.
#[derive(IntoElement)]
pub struct SidebarInput {
    state: Entity<InputState>,
}

impl SidebarInput {
    pub fn new(state: &Entity<InputState>) -> Self {
        Self {
            state: state.clone(),
        }
    }
}

impl RenderOnce for SidebarInput {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let scope = cx.app_theme().sidebar_scope();
        div()
            .flex_1()
            .min_w_0()
            .text_size(TEXT_SM.size)
            .font_weight(FontWeight::MEDIUM)
            .text_color(scope.sidebar_foreground)
            .child(Input::new(&self.state).appearance(false).p_0().h_auto())
    }
}

#[cfg(test)]
mod container_tests {
    use super::*;

    // The inset is `flex-col`, and the app layout passes `flex-row` for the
    // chat column and the right panel. When the component's own classes won,
    // the panel stacked under a zero-height chat column.
    #[test]
    fn parent_classes_override_the_components_own() {
        let mut inset = apply_overrides(
            div().flex().flex_col(),
            vec![Box::new(|this: Div| this.flex_row())],
        );
        let direction = Styled::style(&mut inset).flex_direction;
        assert_eq!(format!("{direction:?}"), "Some(Row)");
    }
}
