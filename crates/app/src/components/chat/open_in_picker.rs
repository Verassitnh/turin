//! Port of upstream/apps/web/src/components/chat/OpenInPicker.tsx
#![allow(dead_code)]

use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _, Window, div, rems,
};
use turin_contracts::editor::EditorId;

use super::super::jetbrains_icons;
use crate::assets::editor_icons;
use crate::assets::PROVIDER_CURSOR;
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenInOptionKind {
    Brand,
    Generic,
}

#[derive(Clone, Debug)]
pub struct OpenInOption {
    pub label: String,
    pub icon_path: SharedString,
    pub value: EditorId,
    pub kind: OpenInOptionKind,
}

fn is_mac_platform(platform: &str) -> bool {
    platform.contains("Mac")
}

fn is_windows_platform(platform: &str) -> bool {
    platform.contains("Win")
}

fn editor_icon_path(editor: EditorId, platform: &str) -> SharedString {
    if let Some(path) = jetbrains_icons::jetbrains_icon_for_editor(editor) {
        return path.into();
    }
    match editor {
        EditorId::Cursor => PROVIDER_CURSOR.into(),
        EditorId::Trae => editor_icons::TRAE.into(),
        EditorId::Kiro => editor_icons::KIRO.into(),
        EditorId::Vscode => editor_icons::VSCODE.into(),
        EditorId::VscodeInsiders => editor_icons::VSCODE_INSIDERS.into(),
        EditorId::Vscodium => editor_icons::VSCODIUM.into(),
        EditorId::Zed => editor_icons::ZED.into(),
        EditorId::Antigravity => editor_icons::ANTIGRAVITY.into(),
        EditorId::FileManager if is_mac_platform(platform) => editor_icons::FINDER.into(),
        EditorId::FileManager if is_windows_platform(platform) => editor_icons::FILE_EXPLORER.into(),
        EditorId::FileManager => editor_icons::FOLDER_CLOSED.into(),
        _ => PROVIDER_CURSOR.into(),
    }
}

pub fn resolve_open_in_options(
    platform: &str,
    available_editors: &[EditorId],
) -> Vec<OpenInOption> {
    let file_manager_kind = if is_mac_platform(platform) || is_windows_platform(platform) {
        OpenInOptionKind::Brand
    } else {
        OpenInOptionKind::Generic
    };
    let mut options = Vec::new();
    let mut push = |editor: EditorId, kind: OpenInOptionKind| {
        if available_editors.contains(&editor) {
            options.push(OpenInOption {
                label: turin_launcher::editor_labels::editor_label_for_platform(editor, platform),
                icon_path: editor_icon_path(editor, platform),
                value: editor,
                kind,
            });
        }
    };
    push(EditorId::Cursor, OpenInOptionKind::Brand);
    push(EditorId::Trae, OpenInOptionKind::Brand);
    push(EditorId::Kiro, OpenInOptionKind::Brand);
    push(EditorId::Vscode, OpenInOptionKind::Brand);
    push(EditorId::VscodeInsiders, OpenInOptionKind::Brand);
    push(EditorId::Vscodium, OpenInOptionKind::Brand);
    push(EditorId::Zed, OpenInOptionKind::Brand);
    push(EditorId::Antigravity, OpenInOptionKind::Brand);
    push(EditorId::Idea, OpenInOptionKind::Brand);
    push(EditorId::Aqua, OpenInOptionKind::Brand);
    push(EditorId::Clion, OpenInOptionKind::Brand);
    push(EditorId::Datagrip, OpenInOptionKind::Brand);
    push(EditorId::Dataspell, OpenInOptionKind::Brand);
    push(EditorId::Goland, OpenInOptionKind::Brand);
    push(EditorId::Phpstorm, OpenInOptionKind::Brand);
    push(EditorId::Pycharm, OpenInOptionKind::Brand);
    push(EditorId::Rider, OpenInOptionKind::Brand);
    push(EditorId::Rubymine, OpenInOptionKind::Brand);
    push(EditorId::Rustrover, OpenInOptionKind::Brand);
    push(EditorId::Webstorm, OpenInOptionKind::Brand);
    push(EditorId::FileManager, file_manager_kind);
    options
}

type OpenHandler = Rc<dyn Fn(EditorId, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct OpenInPicker {
    pub options: Vec<OpenInOption>,
    pub preferred_editor: Option<EditorId>,
    pub open_in_cwd: Option<String>,
    pub can_open: bool,
    pub panel: bool,
    pub on_open: OpenHandler,
    pub on_choose_preferred: OpenHandler,
}

impl RenderOnce for OpenInPicker {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let primary = self.preferred_editor.and_then(|preferred| {
            self.options
                .iter()
                .find(|option| option.value == preferred)
                .cloned()
        });
        let disabled = self.open_in_cwd.is_none() || !self.can_open;
        div()
            .flex()
            .items_center()
            .child(
                Button::new("open-in-primary")
                    .size(if self.panel {
                        ButtonSize::Sm
                    } else {
                        ButtonSize::Xs
                    })
                    .variant(if self.panel {
                        ButtonVariant::Ghost
                    } else {
                        ButtonVariant::Outline
                    })
                    .disabled(primary.is_none() || disabled)
                    .when_some(primary, |this, option| {
                        this.child(Icon::path(option.icon_path).size(rems(0.875)))
                            .label(if self.panel {
                                format!("Open in {}", option.label)
                            } else {
                                "Open".to_owned()
                            })
                    })
                    .on_click({
                        let on_open = self.on_open.clone();
                        let preferred = self.preferred_editor;
                        move |_, window, cx| {
                            if let Some(editor) = preferred {
                                on_open(editor, window, cx);
                            }
                        }
                    }),
            )
            .child(
                Button::new("open-in-menu")
                    .size(if self.panel {
                        ButtonSize::Sm
                    } else {
                        ButtonSize::IconXs
                    })
                    .variant(if self.panel {
                        ButtonVariant::Ghost
                    } else {
                        ButtonVariant::Outline
                    })
                    .icon(IconName::ChevronDown)
                    .disabled(self.options.is_empty()),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn includes_file_manager_with_platform_label() {
        let options =
            resolve_open_in_options("MacIntel", &[EditorId::Vscode, EditorId::FileManager]);
        assert_eq!(options.len(), 2);
        assert!(
            options
                .iter()
                .any(|option| option.value == EditorId::FileManager)
        );
    }

    #[test]
    fn omits_unavailable_editors() {
        assert!(resolve_open_in_options("MacIntel", &[]).is_empty());
    }

    #[test]
    fn maps_each_editor_to_its_own_icon() {
        let options = resolve_open_in_options(
            "MacIntel",
            &[
                EditorId::Vscode,
                EditorId::Zed,
                EditorId::FileManager,
            ],
        );
        let vscode = options
            .iter()
            .find(|option| option.value == EditorId::Vscode)
            .unwrap();
        let zed = options
            .iter()
            .find(|option| option.value == EditorId::Zed)
            .unwrap();
        assert_eq!(vscode.icon_path.as_ref(), editor_icons::VSCODE);
        assert_eq!(zed.icon_path.as_ref(), editor_icons::ZED);
        assert_eq!(
            options
                .iter()
                .find(|option| option.value == EditorId::FileManager)
                .unwrap()
                .icon_path
                .as_ref(),
            editor_icons::FINDER
        );
    }
}
