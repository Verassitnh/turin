//! File-tree glyphs from [`super::pierre_icons`] (`Icon::path` + token tint).

use gpui_kit::{Hsla, Rems};

use crate::theme::color::hex;
use crate::ui::pierre_icons::{
    pierre_icon_asset_path, pierre_icon_token_color, resolve_pierre_icon_for_entry,
};
use crate::ui::{Icon, IconName};

/// Pierre resolver + asset path; neutral file glyph when nothing matches.
pub fn file_tree_file_icon(path: &str, dark: bool, size: Rems) -> Icon {
    let Some(resolved) = resolve_pierre_icon_for_entry(path, false, None) else {
        return Icon::new(IconName::FileText).size(size);
    };
    let Some(asset) = pierre_icon_asset_path(&resolved.name, dark) else {
        return Icon::new(IconName::FileText).size(size);
    };
    let mut icon = Icon::path(asset).size(size);
    if let Some(token) = resolved.token.as_deref()
        && let Some(rgb) = pierre_icon_token_color(token, dark)
    {
        icon = icon.color(hex(rgb));
    }
    icon
}

pub fn file_tree_folder_icon(closed: bool, color: Hsla, size: Rems) -> Icon {
    Icon::new(if closed {
        IconName::Folder
    } else {
        IconName::FolderOpen
    })
    .size(size)
    .color(color)
}
