//! Port of upstream/apps/web/src/components/ui/menu.tsx
//!
//! Menus are gpui-component's `PopupMenu` (opened with `DropdownMenu` or
//! `ContextMenuExt`), which already draws upstream's shape: `p-1` list,
//! `rounded-sm` rows with `bg-accent` highlight, separators and shortcut
//! hints. Its colors and radius come from the theme projection in
//! `theme::component_theme`. Upstream → here:
//!
//! | upstream            | gpui-component                         |
//! |---------------------|----------------------------------------|
//! | `Menu`+`MenuTrigger`| `DropdownMenu::dropdown_menu`           |
//! | `MenuItem`          | `PopupMenu::menu` / `PopupMenuItem`     |
//! | `MenuCheckboxItem`  | `PopupMenu::menu_with_check`            |
//! | `MenuSeparator`     | `PopupMenu::separator`                  |
//! | `MenuGroupLabel`    | `PopupMenu::label`                      |
//! | `MenuSub*`          | `PopupMenu::submenu`                    |
//!
//! [`MenuShortcut`] and [`menu_group_label`] are for custom (`element`) rows.

#[allow(unused_imports)]
pub use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem};

use gpui_kit::{
    App, Div, FontWeight, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _,
    Window, div, rems,
};

use crate::theme::ActiveAppTheme;
use crate::theme::tokens::TEXT_XS;

/// `MenuShortcut`: `ms-auto font-medium text-secondary-label text-xs tracking-widest`.
#[derive(IntoElement)]
pub struct MenuShortcut(SharedString);

impl MenuShortcut {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self(label.into())
    }
}

impl RenderOnce for MenuShortcut {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        div()
            .ml_auto()
            .font_weight(FontWeight::MEDIUM)
            .text_size(TEXT_XS.size)
            .text_color(cx.app_theme().secondary_label)
            .child(self.0)
    }
}

/// `MenuGroupLabel`: `px-2 py-1.5 font-medium text-muted-foreground text-xs`.
pub fn menu_group_label(label: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .px_2()
        .py(rems(0.375))
        .font_weight(FontWeight::MEDIUM)
        .text_size(TEXT_XS.size)
        .text_color(cx.app_theme().muted_foreground)
        .child(label.into())
}
