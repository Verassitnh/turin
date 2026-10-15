//! Port of upstream/apps/web/src/components/settings/settingsLayout.tsx and
//! SettingsGroup.tsx: the section / group / row frame every panel is built
//! from.
//!
//! Not ported: the scope machinery (`SettingInheritance`, `ScopedSwitch`,
//! scope sentence), which only exists to retarget settings at remote
//! environments and projects; see PORTING.md Deviations.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Div, ElementId, FontWeight, Global, Hsla, InteractiveElement as _,
    IntoElement, ParentElement as _, RenderOnce, SharedString, Stateful, Styled as _, Window, div,
    px, rems,
};

use crate::theme::color::with_alpha;
use crate::theme::tokens::{self, RADIUS_XL, TEXT_SM, TEXT_XS};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::{Button, ButtonSize, ButtonVariant, IconName, Tooltip};

/// Width of the settings content column, published by the shell each frame.
/// Rows switch between the stacked and the two-column layout the way
/// upstream's `@min-[32rem]/settings-row` container query does.
#[derive(Clone, Copy, Debug)]
pub struct SettingsMetrics {
    /// Width available to a row (after the page padding), in px.
    pub row_width: f32,
}

impl Default for SettingsMetrics {
    fn default() -> Self {
        Self { row_width: 800. }
    }
}

impl Global for SettingsMetrics {}

/// `@min-[32rem]/settings-row`
const ROW_WIDE_MIN_WIDTH: f32 = 512.;

/// The settings-search destination currently pulsing (a row or section id).
#[derive(Clone, Debug, Default)]
pub struct SettingsSearchTarget {
    pub id: Option<SharedString>,
}

impl Global for SettingsSearchTarget {}

fn is_search_target(id: &Option<SharedString>, cx: &App) -> bool {
    match (id, cx.try_global::<SettingsSearchTarget>()) {
        (Some(id), Some(target)) => target.id.as_ref() == Some(id),
        _ => false,
    }
}

/// `settings-search-target-pulse`: `inset 0 0 0 2px primary/70`.
fn pulse_ring(theme: &AppTheme) -> Hsla {
    with_alpha(theme.primary, 0.7).into()
}

/// `SettingsGroup variant="grouped"`: `rounded-xl border border-border/60
/// bg-card/40 shadow-xs/5`, rows separated by `border-border/50`.
pub fn settings_group(children: Vec<AnyElement>, cx: &App) -> Div {
    let theme = cx.app_theme();
    let separator: Hsla = with_alpha(theme.border, 0.5).into();
    div()
        .relative()
        .flex()
        .flex_col()
        .rounded(RADIUS_XL)
        .border_1()
        .border_color(with_alpha(theme.border, 0.6))
        .bg(with_alpha(theme.card, 0.4))
        .shadow(tokens::shadow_xs(0.05))
        .text_color(theme.foreground)
        .children(children.into_iter().enumerate().map(move |(index, child)| {
            div()
                .when(index > 0, |this| this.border_t_1().border_color(separator))
                .child(child)
        }))
}

/// `variant="plain"`: `space-y-1`.
pub fn settings_group_plain(children: Vec<AnyElement>) -> Div {
    div().flex().flex_col().gap_1().children(children)
}

#[derive(IntoElement)]
pub struct SettingsSection {
    id: Option<SharedString>,
    title: SharedString,
    hide_title: bool,
    icon: Option<AnyElement>,
    title_action: Option<AnyElement>,
    header_action: Option<AnyElement>,
    plain: bool,
    children: Vec<AnyElement>,
}

impl SettingsSection {
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self {
            id: None,
            title: title.into(),
            hide_title: false,
            icon: None,
            title_action: None,
            header_action: None,
            plain: false,
            children: Vec::new(),
        }
    }

    pub fn id(mut self, id: impl Into<SharedString>) -> Self {
        self.id = Some(id.into());
        self
    }

    pub fn hide_title(mut self) -> Self {
        self.hide_title = true;
        self
    }

    pub fn icon(mut self, icon: impl IntoElement) -> Self {
        self.icon = Some(icon.into_any_element());
        self
    }

    /// Small control right after the title, such as an add button.
    pub fn title_action(mut self, action: impl IntoElement) -> Self {
        self.title_action = Some(action.into_any_element());
        self
    }

    pub fn header_action(mut self, action: impl IntoElement) -> Self {
        self.header_action = Some(action.into_any_element());
        self
    }

    pub fn plain(mut self) -> Self {
        self.plain = true;
        self
    }
}

impl gpui_kit::ParentElement for SettingsSection {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for SettingsSection {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let pulsing = is_search_target(&self.id, cx);
        let group = if self.plain {
            settings_group_plain(self.children)
        } else {
            settings_group(self.children, cx)
        };
        div()
            .flex()
            .flex_col()
            .when(!self.hide_title, |this| this.gap(rems(0.625)))
            .when(!self.hide_title, |this| {
                let mut header = div()
                    .flex()
                    .min_h_7()
                    .items_start()
                    .justify_between()
                    .gap_4()
                    .px_4()
                    .child(
                        div()
                            .flex()
                            .min_w_0()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .flex()
                                    .min_h_7()
                                    .items_center()
                                    .gap_2()
                                    .text_size(TEXT_SM.size)
                                    .line_height(TEXT_SM.line_height)
                                    .font_weight(FontWeight::NORMAL)
                                    .text_color(with_alpha(theme.foreground, 0.7))
                                    .children(self.icon)
                                    .child(self.title.clone()),
                            )
                            .children(self.title_action),
                    )
                    .child(
                        div()
                            .flex()
                            .min_h_7()
                            .min_w_7()
                            .items_center()
                            .justify_end()
                            .children(self.header_action),
                    );
                if pulsing {
                    header = header
                        .rounded(RADIUS_XL)
                        .border_2()
                        .border_color(pulse_ring(theme));
                }
                this.child(header)
            })
            .child(group)
    }
}

/// `SettingResetButton`: `icon-micro` ghost-muted undo arrow.
pub fn setting_reset_button(
    id: impl Into<ElementId>,
    tooltip: &'static str,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> Button {
    Button::new(id)
        .variant(ButtonVariant::GhostMuted)
        .size(ButtonSize::IconMicro)
        .icon(IconName::Undo2)
        .icon_size(rems(0.75))
        .tooltip(Tooltip::new(tooltip))
        .on_click(move |_, window, cx| on_click(window, cx))
}

/// One setting row. See the doc comment in upstream settingsLayout.tsx for the
/// control sizing tiers.
#[derive(IntoElement)]
pub struct SettingsRow {
    id: Option<SharedString>,
    title: AnyElement,
    description: Option<AnyElement>,
    status: Option<AnyElement>,
    reset_action: Option<AnyElement>,
    control: Option<AnyElement>,
    disabled: bool,
    children: Vec<AnyElement>,
}

impl SettingsRow {
    pub fn new(title: impl IntoElement) -> Self {
        Self {
            id: None,
            title: title.into_any_element(),
            description: None,
            status: None,
            reset_action: None,
            control: None,
            disabled: false,
            children: Vec::new(),
        }
    }

    pub fn id(mut self, id: impl Into<SharedString>) -> Self {
        self.id = Some(id.into());
        self
    }

    pub fn description(mut self, description: impl IntoElement) -> Self {
        self.description = Some(description.into_any_element());
        self
    }

    pub fn status(mut self, status: impl IntoElement) -> Self {
        self.status = Some(status.into_any_element());
        self
    }

    pub fn reset_action(mut self, action: Option<impl IntoElement>) -> Self {
        self.reset_action = action.map(IntoElement::into_any_element);
        self
    }

    pub fn control(mut self, control: impl IntoElement) -> Self {
        self.control = Some(control.into_any_element());
        self
    }

    /// `aria-disabled`: `opacity-64`.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl gpui_kit::ParentElement for SettingsRow {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for SettingsRow {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let wide = cx
            .try_global::<SettingsMetrics>()
            .copied()
            .unwrap_or_default()
            .row_width
            >= ROW_WIDE_MIN_WIDTH;
        let has_children = !self.children.is_empty();
        let pulsing = is_search_target(&self.id, cx);

        let slot = |content: Option<AnyElement>| {
            div()
                .flex()
                .flex_shrink_0()
                .size_5()
                .items_center()
                .justify_center()
                .children(content)
        };

        let left = div()
            .flex()
            .flex_col()
            .gap_1()
            .min_w_0()
            .flex_1()
            .child(
                div()
                    .flex()
                    .min_h_5()
                    .items_center()
                    .gap(rems(0.375))
                    .child(
                        div()
                            .text_size(TEXT_SM.size)
                            .line_height(TEXT_SM.line_height)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.foreground)
                            .child(self.title),
                    )
                    // The inheritance indicator slot, empty without scopes.
                    .child(slot(None))
                    .child(slot(self.reset_action)),
            )
            .children(self.description.map(|description| {
                div()
                    .max_w(rems(36.))
                    .text_size(TEXT_XS.size)
                    .line_height(rems(1.25))
                    .text_color(with_alpha(theme.muted_foreground, 0.8))
                    .child(description)
            }))
            .children(self.status.map(|status| {
                div()
                    .pt(px(2.))
                    .text_size(TEXT_XS.size)
                    .line_height(TEXT_XS.line_height)
                    .text_color(theme.muted_foreground)
                    .child(status)
            }));

        let control = self.control.map(|control| {
            div()
                .flex()
                .min_w_0()
                .flex_shrink_0()
                .items_center()
                .gap_2()
                .when(wide, |this| this.justify_end().min_w(rems(10.)))
                .when(!wide, |this| this.w_full())
                .child(control)
        });

        let main = if wide {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_8()
                .child(left)
                .children(control)
        } else {
            div()
                .flex()
                .flex_col()
                .gap_3()
                .child(left)
                .children(control)
        };

        div()
            .id(ElementId::Name(
                self.id.clone().unwrap_or_else(|| "settings-row".into()),
            ))
            .flex()
            .flex_col()
            .px_4()
            .when(has_children, |this| this.pt_3().pb_1())
            .when(!has_children, |this| this.py_3())
            .when(self.disabled, |this| this.opacity(0.64))
            .when(pulsing, |this| {
                this.border_2().border_color(pulse_ring(theme))
            })
            .child(main)
            .children(self.children)
    }
}

/// `SettingsUnavailableGroup`: a notice above dimmed rows.
pub fn settings_unavailable_group(
    message: Option<SharedString>,
    children: Vec<AnyElement>,
    cx: &App,
) -> Div {
    let theme = cx.app_theme();
    match message {
        None => div().flex().flex_col().children(children),
        Some(message) => div()
            .py(rems(0.375))
            .bg(with_alpha(theme.muted, 0.2))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .px_4()
                    .py_2()
                    .text_size(TEXT_XS.size)
                    .line_height(rems(1.25))
                    .text_color(theme.muted_foreground)
                    .child(
                        crate::ui::Icon::new(IconName::Info)
                            .size(rems(0.875))
                            .color(theme.warning),
                    )
                    .child(div().child(message)),
            )
            .child(div().opacity(0.64).flex().flex_col().children(children)),
    }
}

/// Helper: a stateful div with an id, for panel code that needs listeners.
pub fn clickable(id: impl Into<ElementId>) -> Stateful<Div> {
    div().id(id)
}
