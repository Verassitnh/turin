//! Port of upstream/apps/web/src/components/settings/ProjectFaviconPickerDialog.tsx
//! and upstream/apps/web/src/components/settings/ProjectFaviconPickerDialog.test.tsx.
//!
//! "Choose project icon" from an image file in the project: a command-palette
//! style search over the project's image files, with an optional "Open in
//! Finder" action for a file outside the project.
//!
//! Deviations: results come synchronously from
//! `SettingsSource::search_project_files` (no indexing/searching states); file
//! rows show a file glyph, not the `@pierre/diffs` entry icon (the file tree
//! icons are the file browser's).

use gpui_kit::{
    AnyElement, Context, CursorStyle, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    deferred, div, rems,
};

use super::dialog::Dialog;
use super::project_file_picker_logic::{
    PROJECT_FILE_PICKER_RESULT_LIMIT, get_project_file_picker_matches,
};
use super::view::{SettingsToastKind, SettingsView};
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{TEXT_SM, TEXT_XS};
use crate::ui::command::{
    FooterKey, command_dialog_popup, command_footer, command_footer_hint, command_input_row,
    command_item_frame, command_list_heading, command_panel, dialog_backdrop,
};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName};

/// `canPickExternalProjectFavicon`: a native picker needs a path the host can
/// open; WSL paths on Windows hosts cannot be.
pub fn can_pick_external_project_favicon(cwd: &str, platform: &str) -> bool {
    let is_windows = platform.to_lowercase().contains("win");
    let is_windows_absolute = {
        let bytes = cwd.as_bytes();
        (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'\\' || bytes[2] == b'/'))
            || cwd.starts_with("\\\\")
    };
    !is_windows || is_windows_absolute
}

/// `getLocalFileManagerName`.
pub fn local_file_manager_name(platform: &str) -> &'static str {
    let platform = platform.to_lowercase();
    if platform.contains("mac") {
        "Finder"
    } else if platform.contains("win") {
        "Explorer"
    } else {
        "Files"
    }
}

/// The dialog's state and target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaviconPicker {
    pub cwd: String,
    pub project_name: String,
    pub project_ids: Vec<String>,
    pub query: String,
    pub can_pick_external: bool,
}

/// `emptyMessage`.
pub fn empty_message(query: &str, error: Option<&str>) -> String {
    match error {
        Some(error) => error.to_owned(),
        None if query.trim().is_empty() => "No image files found.".to_owned(),
        None => "No matching image files.".to_owned(),
    }
}

impl SettingsView {
    pub(super) fn open_project_favicon_picker(
        &mut self,
        project_ids: Vec<String>,
        project_name: String,
        cwd: String,
        can_pick_external: bool,
        cx: &mut Context<Self>,
    ) {
        self.panels.projects.favicon_picker = Some(FaviconPicker {
            cwd,
            project_name,
            project_ids,
            query: String::new(),
            can_pick_external,
        });
        self.dialog = Some(Dialog::ProjectFaviconPicker);
        cx.notify();
    }

    fn close_favicon_picker(&mut self, cx: &mut Context<Self>) {
        self.dialog = None;
        self.panels.projects.favicon_picker = None;
        cx.notify();
    }

    /// `ProjectFaviconPickerDialog`.
    pub(super) fn render_project_favicon_picker(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = cx.app_theme().clone();
        let picker = self.panels.projects.favicon_picker.clone()?;
        let source = self.store.read(cx).source();
        let (entries, error) = match source.search_project_files(&picker.cwd, &picker.query, cx) {
            Ok(entries) => (entries, None),
            Err(message) => (Vec::new(), Some(message)),
        };
        let matches = get_project_file_picker_matches(
            &entries,
            &picker.query,
            PROJECT_FILE_PICKER_RESULT_LIMIT,
        );
        let input = self.input_live(
            "favicon-picker-query",
            &picker.query,
            "Search image files…",
            window,
            cx,
            |this, text, _, cx| {
                if let Some(picker) = this.panels.projects.favicon_picker.as_mut() {
                    picker.query = text;
                }
                cx.notify();
            },
        );
        let search_input = gpui_kit::component::input::Input::new(&input)
            .appearance(false)
            .bordered(false)
            .focus_bordered(false);

        let mut list = div().flex().flex_col().p_1();
        if matches.is_empty() {
            list = list.child(
                div()
                    .py(rems(3.))
                    .text_center()
                    .text_size(TEXT_SM.size)
                    .text_color(theme.muted_foreground)
                    .child(empty_message(&picker.query, error.as_deref())),
            );
        } else {
            list = list.child(command_list_heading(picker.project_name.clone(), cx));
            for (index, file) in matches.into_iter().enumerate() {
                let path = file.path.clone();
                let project_ids = picker.project_ids.clone();
                list = list.child(
                    command_item_frame(false, false, cx)
                        .id(SharedString::from(format!("favicon-file-{index}")))
                        .cursor(CursorStyle::PointingHand)
                        .hover(|style| style.bg(theme.accent))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_favicon_picker(cx);
                            this.set_project_icon(
                                project_ids.clone(),
                                Some(path.clone()),
                                None,
                                cx,
                            );
                        }))
                        .child(
                            Icon::new(IconName::File)
                                .size(rems(1.))
                                .color(theme.icon_muted),
                        )
                        .child(
                            div()
                                .flex()
                                .min_w_0()
                                .flex_1()
                                .items_baseline()
                                .gap_2()
                                .child(div().truncate().child(file.name.clone()))
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(TEXT_XS.size)
                                        .text_color(theme.muted_foreground)
                                        .child(file.path.clone()),
                                ),
                        ),
                );
            }
        }

        let external = picker.can_pick_external.then(|| {
            let cwd = picker.cwd.clone();
            let project_ids = picker.project_ids.clone();
            Button::new("favicon-picker-external")
                .variant(ButtonVariant::GhostMuted)
                .size(ButtonSize::Xs)
                .label(format!("Open in {}", local_file_manager_name("MacIntel")))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let source = this.store.read(cx).source();
                    match source.pick_project_favicon(&cwd, cx) {
                        Ok(Some(path)) => {
                            this.close_favicon_picker(cx);
                            this.set_project_icon(project_ids.clone(), Some(path), None, cx);
                        }
                        Ok(None) => {}
                        Err(message) => this.toast(
                            SettingsToastKind::Error,
                            "Could not open image picker",
                            if message.is_empty() {
                                "An error occurred.".to_owned()
                            } else {
                                message
                            },
                            cx,
                        ),
                    }
                }))
        });
        let footer = command_footer(cx)
            .child(command_footer_hint(
                vec![FooterKey::Icon(IconName::CornerDownLeft)],
                "Select icon",
                cx,
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(external)
                    .child(command_footer_hint(
                        vec![FooterKey::Text("Esc".into())],
                        "Close",
                        cx,
                    )),
            );
        let popup = command_dialog_popup(&theme)
            .id("project-favicon-picker")
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(command_input_row(None, search_input, None, cx))
            .child(
                command_panel().child(
                    div()
                        .id("favicon-picker-list")
                        .max_h(rems(16.))
                        .overflow_y_scroll()
                        .child(list),
                ),
            )
            .child(footer);
        let top = window.viewport_size().height * 0.1;
        Some(
            deferred(
                div()
                    .id("project-favicon-picker-backdrop")
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .occlude()
                    .flex()
                    .flex_col()
                    .items_center()
                    .px_4()
                    .pt(top)
                    .bg(dialog_backdrop(&theme))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.close_favicon_picker(cx)),
                    )
                    .child(div().w_full().max_w(rems(36.)).child(popup)),
            )
            .with_priority(10)
            .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hides_the_native_picker_for_wsl_project_paths() {
        assert!(!can_pick_external_project_favicon(
            "/home/me/project",
            "Win32"
        ));
        assert!(can_pick_external_project_favicon(
            "C:\\Users\\me\\project",
            "Win32"
        ));
        assert!(can_pick_external_project_favicon(
            "/Users/me/project",
            "MacIntel"
        ));
    }

    #[test]
    fn names_the_file_manager_by_platform() {
        assert_eq!(local_file_manager_name("MacIntel"), "Finder");
        assert_eq!(local_file_manager_name("Win32"), "Explorer");
        assert_eq!(local_file_manager_name("Linux x86_64"), "Files");
    }

    #[test]
    fn explains_an_empty_list() {
        assert_eq!(empty_message("", None), "No image files found.");
        assert_eq!(empty_message("logo", None), "No matching image files.");
        assert_eq!(
            empty_message("logo", Some("Index failed.")),
            "Index failed."
        );
    }
}
