//! Port of upstream/apps/web/src/projectIconColors.ts
//!
//! `PROJECT_ICON_COLORS`: each project color is `text-{c}-600
//! dark:text-{c}-400` (Tailwind v4 palette), with a `bg-{c}-500` swatch.
// A faithful port keeps upstream's whole API (rule 3); callers that are
// not ported yet (T14 reducers, the chat header menu, settings) use the rest.
#![allow(dead_code)]

use gpui_kit::Hsla;

use crate::state::models::ProjectIconColor;
use crate::theme::color::hex;

pub struct ProjectIconColorEntry {
    pub value: ProjectIconColor,
    pub label: &'static str,
    /// `text-{c}-600`.
    pub light: u32,
    /// `dark:text-{c}-400`.
    pub dark: u32,
}

macro_rules! colors {
    ($($value:ident $label:literal $light:literal $dark:literal),* $(,)?) => {
        pub const PROJECT_ICON_COLORS: &[ProjectIconColorEntry] = &[
            $(ProjectIconColorEntry {
                value: ProjectIconColor::$value,
                label: $label,
                light: $light,
                dark: $dark,
            }),*
        ];
    };
}

colors!(
    Gray "Gray" 0x4a5565 0x99a1af,
    Red "Red" 0xe7000b 0xff6467,
    Orange "Orange" 0xf54900 0xff8904,
    Amber "Amber" 0xe17100 0xffb900,
    Yellow "Yellow" 0xd08700 0xfdc700,
    Lime "Lime" 0x5ea500 0x9ae600,
    Green "Green" 0x00a63e 0x05df72,
    Emerald "Emerald" 0x009966 0x00d492,
    Teal "Teal" 0x009689 0x00d5be,
    Cyan "Cyan" 0x0092b8 0x00d3f2,
    Sky "Sky" 0x0084d1 0x00bcff,
    Blue "Blue" 0x155dfc 0x51a2ff,
    Indigo "Indigo" 0x4f39f6 0x7c86ff,
    Violet "Violet" 0x7f22fe 0xa684ff,
    Purple "Purple" 0x9810fa 0xc27aff,
    Fuchsia "Fuchsia" 0xc800de 0xed6bff,
    Pink "Pink" 0xe60076 0xfb64b6,
    Rose "Rose" 0xec003f 0xff637e,
);

/// `projectIconColorClassName(color)` resolved for the appearance.
pub fn project_icon_color(color: ProjectIconColor, dark: bool) -> Hsla {
    let entry = PROJECT_ICON_COLORS
        .iter()
        .find(|entry| entry.value == color)
        .unwrap_or(&PROJECT_ICON_COLORS[11]);
    hex(if dark { entry.dark } else { entry.light }).into()
}
