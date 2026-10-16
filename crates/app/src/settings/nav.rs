//! Port of upstream/apps/web/src/components/settings/SettingsSidebarNav.tsx.

use gpui_kit::component::input::Input;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, CursorStyle, ElementId, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    px, rems,
};

use super::search::{
    SettingsPath, SettingsSearchAvailability, filter_available_settings_search_items,
    is_settings_overview_visible, search_settings,
};
use super::view::{SettingsRoute, SettingsView};
use crate::theme::color::with_alpha;
use crate::theme::tokens::{
    CONTROL_RADIUS, RADIUS_LG, SIDEBAR_CONTENT_INSET, SIDEBAR_ROW_CONTENT_INSET, TEXT_2XS, TEXT_SM,
    TEXT_XS,
};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName, Kbd, scroll_area_hidden};

fn section_icon(path: SettingsPath) -> IconName {
    match path {
        SettingsPath::Projects => IconName::PanelsTopLeft,
        SettingsPath::General => IconName::Settings2,
        SettingsPath::Appearance => IconName::Palette,
        SettingsPath::Keybindings => IconName::Keyboard,
        SettingsPath::Providers => IconName::Bot,
        SettingsPath::SourceControl => IconName::GitBranch,
        SettingsPath::Storage => IconName::HardDrive,
        SettingsPath::Archived => IconName::Archive,
    }
}

/// `sidebarMenuButtonVariants` `default` size: `h-8 rounded-[var(--control-radius)]
/// px-[var(--sidebar-row-content-inset)] py-1.5 text-sm`, `font-medium
/// text-sidebar-muted-foreground/80`; selected rows `bg-sidebar-row-selected
/// text-sidebar-foreground`.
fn menu_button(
    id: impl Into<ElementId>,
    active: bool,
    theme: &AppTheme,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id)
        .flex()
        .w_full()
        .h_8()
        .items_center()
        .gap(rems(0.5))
        .overflow_hidden()
        .rounded(CONTROL_RADIUS)
        .px(SIDEBAR_ROW_CONTENT_INSET)
        .py(rems(0.375))
        .text_size(TEXT_SM.size)
        .font_weight(FontWeight::MEDIUM)
        .cursor(CursorStyle::PointingHand)
        .text_color(if active {
            theme.sidebar_foreground
        } else {
            with_alpha(theme.sidebar_muted_foreground, 0.8).into()
        })
        .when(active, |this| this.bg(theme.sidebar_row_selected))
        .when(!active, |this| {
            this.hover(|style| {
                style
                    .bg(theme.sidebar_row_hover)
                    .text_color(theme.sidebar_foreground)
            })
        })
        .active(|style| {
            style
                .bg(theme.sidebar_row_active)
                .text_color(theme.sidebar_foreground)
        })
}

impl SettingsView {
    pub(super) fn render_nav(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let query = self.nav_query.clone();
        let searching = !query.trim().is_empty();
        let vm = self.vm(cx);
        let items = filter_available_settings_search_items(&SettingsSearchAvailability::default());
        let results = if searching {
            search_settings(&query, &items, &vm.platform)
        } else {
            Vec::new()
        };

        let search_row = div()
            .flex()
            .h_8()
            .items_center()
            .gap_2()
            .rounded_md()
            .px_2()
            .py(rems(0.375))
            .text_size(TEXT_SM.size)
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme.sidebar_muted_foreground)
            .hover(|style| {
                style
                    .bg(theme.sidebar_row_hover)
                    .text_color(theme.sidebar_foreground)
            })
            .child(
                Icon::new(IconName::Search)
                    .size(rems(1.))
                    .color(with_alpha(theme.sidebar_muted_foreground, 0.8).into()),
            )
            .child(
                div().min_w_0().flex_1().child(
                    Input::new(&self.nav_search)
                        .appearance(false)
                        .bordered(false)
                        .focus_bordered(false)
                        .text_size(TEXT_SM.size),
                ),
            )
            .child(if searching {
                let clear = cx.listener(|this, _, window, cx| {
                    this.nav_query.clear();
                    this.nav_search
                        .update(cx, |state, cx| state.set_value("", window, cx));
                    cx.notify();
                });
                Button::new("clear-settings-search")
                    .variant(ButtonVariant::GhostMuted)
                    .size(ButtonSize::IconMicro)
                    .icon(IconName::X)
                    .icon_size(rems(0.75))
                    .on_click(clear)
                    .into_any_element()
            } else {
                Kbd::new("/").into_any_element()
            });

        let context = self.scope_context(cx);
        let nav_paths: Vec<SettingsPath> = SettingsPath::ALL
            .into_iter()
            .filter(|path| {
                *path != SettingsPath::Projects
                    || is_settings_overview_visible(context.search_kind())
            })
            .collect();
        let list: AnyElement = if searching {
            if results.is_empty() {
                div()
                    .px_2()
                    .py(rems(1.5))
                    .text_center()
                    .text_size(TEXT_XS.size)
                    .text_color(theme.sidebar_muted_foreground)
                    .child("No settings found")
                    .into_any_element()
            } else {
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(results.into_iter().enumerate().map(|(index, item)| {
                        let target = item.target_id.clone().unwrap_or_else(|| item.id.clone());
                        let route = SettingsRoute::Section(item.to);
                        menu_button(
                            SharedString::from(format!("settings-result-{}", item.id)),
                            index == 0,
                            &theme,
                        )
                        .h_auto()
                        .min_h_10()
                        .items_start()
                        .rounded_lg()
                        .p_2()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_search_target(route, target.clone(), cx);
                        }))
                        .child(
                            div().mt(px(2.)).child(
                                Icon::new(section_icon(item.to))
                                    .size(rems(0.875))
                                    .color(with_alpha(theme.sidebar_muted_foreground, 0.6).into()),
                            ),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(TEXT_SM.size)
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme.sidebar_foreground)
                                        .child(item.title.clone()),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(TEXT_2XS.size)
                                        .font_weight(FontWeight::NORMAL)
                                        .text_color(with_alpha(
                                            theme.sidebar_muted_foreground,
                                            0.75,
                                        ))
                                        .child(item.to.label()),
                                ),
                        )
                    }))
                    .into_any_element()
            }
        } else {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .children(nav_paths.into_iter().map(|path| {
                    let route = SettingsRoute::Section(path);
                    let active = self.route == route
                        || (path == SettingsPath::General
                            && self.route == SettingsRoute::OpenSourceLicenses);
                    menu_button(
                        SharedString::from(format!("settings-nav-{}", path.path())),
                        active,
                        &theme,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.set_route(route, cx)))
                    .child(
                        Icon::new(section_icon(path))
                            .size(rems(1.))
                            .color(if active {
                                theme.sidebar_foreground
                            } else {
                                theme.sidebar_icon_color
                            }),
                    )
                    .child(div().truncate().child(path.label()))
                }))
                .into_any_element()
        };

        div()
            .flex_1()
            .min_h_0()
            .child(
                scroll_area_hidden("settings-nav").child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .p(SIDEBAR_CONTENT_INSET)
                        .child(search_row)
                        .child(list),
                ),
            )
            .into_any_element()
    }

    /// `handleSearchResultClick`: open the page and pulse the matched row.
    pub(super) fn open_search_target(
        &mut self,
        route: SettingsRoute,
        target: String,
        cx: &mut Context<Self>,
    ) {
        self.nav_query.clear();
        self.route = route;
        self.menus.close();
        cx.set_global(super::layout::SettingsSearchTarget {
            id: Some(target.into()),
        });
        cx.notify();
    }
}

#[allow(dead_code)]
fn _radius(_: gpui_kit::Pixels) {
    let _ = RADIUS_LG;
}
