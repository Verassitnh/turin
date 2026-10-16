//! Port of upstream/apps/web/src/components/JetBrainsIcons.tsx
#![allow(dead_code)]
//!
//! Monochrome-friendly brand marks live under `assets/editors/`.

pub use crate::assets::editor_icons::{
    AQUA, CLION, DATAGRIP, DATASPELL, GOLAND, IDEA, PHPSTORM, PYCHARM, RIDER, RUBYMINE, RUSTROVER,
    WEBSTORM,
};

pub const JETBRAINS_ICON_PATHS: &[&str] = &[
    AQUA, CLION, DATAGRIP, DATASPELL, GOLAND, IDEA, PHPSTORM, PYCHARM, RIDER, RUBYMINE, RUSTROVER,
    WEBSTORM,
];

use turin_contracts::editor::EditorId;

pub fn jetbrains_icon_for_editor(editor: EditorId) -> Option<&'static str> {
    match editor {
        EditorId::Idea => Some(IDEA),
        EditorId::Aqua => Some(AQUA),
        EditorId::Clion => Some(CLION),
        EditorId::Datagrip => Some(DATAGRIP),
        EditorId::Dataspell => Some(DATASPELL),
        EditorId::Goland => Some(GOLAND),
        EditorId::Phpstorm => Some(PHPSTORM),
        EditorId::Pycharm => Some(PYCHARM),
        EditorId::Rider => Some(RIDER),
        EditorId::Rubymine => Some(RUBYMINE),
        EditorId::Rustrover => Some(RUSTROVER),
        EditorId::Webstorm => Some(WEBSTORM),
        _ => None,
    }
}
