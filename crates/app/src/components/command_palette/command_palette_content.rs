//! Port of upstream/apps/web/src/components/CommandPaletteContent.tsx plus
//! the dialog shell from CommandPalette.tsx (`CommandDialogPopup`) and the
//! palette-level pieces around the results: the Add/Create/Add-project submit
//! button, the project-name preview, loading/error status lines and the
//! footer's "Open in Finder" action.
//!
//! One input, panel and keyboard-hint gutter shared by every palette mode.

use gpui_kit::component::input::Input;
use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Window, div, px, rems,
};

use super::command_palette_logic::{CommandPaletteMode, SearchOverlayMode};
use super::{CommandPalette, KEY_CONTEXT};
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_SM, TEXT_SM, TEXT_XS};
use crate::ui::command::{
    FooterKey, command_dialog_popup, command_footer, command_footer_hint, command_input_row,
    command_panel, dialog_backdrop,
};
use crate::ui::icon::{Icon, IconName};
use crate::ui::{Button, ButtonSize, ButtonVariant, Kbd, KbdGroup, Tooltip};

impl CommandPalette {
    /// `CommandDialogPopup` over its backdrop, hosting the active mode.
    pub(crate) fn render_overlay(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        // `sm:py-[10vh]`.
        let top = window.viewport_size().height * 0.1;
        let mode = self.ui.mode;
        let body = match mode {
            SearchOverlayMode::Command => self.render_command_content(window, cx),
            SearchOverlayMode::Files => self.render_unavailable_mode(
                "File picker",
                "Go to file arrives with the project file picker.",
                cx,
            ),
            SearchOverlayMode::Content => self.render_unavailable_mode(
                "Search project contents",
                "Project content search arrives with its own task.",
                cx,
            ),
        };
        div()
            .id("command-palette")
            .key_context(KEY_CONTEXT)
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .px_4()
            .pt(top)
            .bg(dialog_backdrop(&theme))
            .capture_key_down(
                cx.listener(|this, event, window, cx| this.on_key_down(event, window, cx)),
            )
            // `onBackdropPointerDown`.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.set_open(false, window, cx)),
            )
            .child(
                command_dialog_popup(&theme)
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(body),
            )
            .into_any_element()
    }

    /// File picker / content search placeholders: the same chrome with a
    /// status line instead of a list.
    fn render_unavailable_mode(
        &mut self,
        title: &'static str,
        message: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        div()
            .flex()
            .flex_col()
            .child(command_input_row(
                None,
                div().text_color(theme.foreground).child(title),
                None,
                cx,
            ))
            .child(
                command_panel().child(
                    div()
                        .py(rems(2.5))
                        .flex()
                        .justify_center()
                        .text_size(TEXT_SM.size)
                        .text_color(theme.muted_foreground)
                        .child(message),
                ),
            )
            .child(
                command_footer(cx).child(div().flex().items_center().gap_3().child(
                    command_footer_hint(vec![FooterKey::Text("Esc".into())], "Close", cx),
                )),
            )
            .into_any_element()
    }

    /// `OpenCommandPaletteDialog` → `CommandPaletteContent`.
    fn render_command_content(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let placeholder = self.placeholder();
        if self.placeholder_shown != placeholder {
            self.placeholder_shown = placeholder;
            self.input.update(cx, |input, cx| {
                input.set_placeholder(placeholder, window, cx)
            });
        }
        let mode = self.mode_kind();
        let is_submenu = matches!(
            mode,
            CommandPaletteMode::Submenu | CommandPaletteMode::SubmenuBrowse
        );
        let path = self.browse_path();
        let is_browsing = path.is_browsing;
        let new_project = self.new_project_flow.is_some();
        let has_highlighted_browse_item = self
            .highlighted
            .as_deref()
            .is_some_and(|value| value.starts_with("browse:"));
        let has_highlighted_new_project_item = new_project && self.highlighted.is_some();
        let relative_blocked = self.relative_path_needs_active_project();
        let can_submit_browse_path = is_browsing && !relative_blocked;
        let will_create = self.will_create_project_path();

        // Start addon: the back button in a submenu, the folder glyph while
        // browsing, else the search icon.
        let start_addon: Option<AnyElement> = if is_submenu {
            Some(
                div()
                    .id("command-palette-back")
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, window, cx| this.pop_view(window, cx)))
                    .child(Icon::new(IconName::ArrowLeft).color(theme.foreground))
                    .into_any_element(),
            )
        } else if is_browsing {
            Some(
                Icon::new(IconName::FolderPlus)
                    .color(theme.foreground)
                    .into_any_element(),
            )
        } else {
            None
        };

        // The submit button sits over the field's end, so the input reserves
        // room for its label.
        let input_end_padding = if has_highlighted_new_project_item {
            9.5
        } else if new_project {
            8.
        } else if is_browsing {
            super::command_palette_logic::browse_input_end_padding(
                will_create,
                has_highlighted_browse_item,
            )
        } else {
            0.
        };

        let submit_modifier = "\u{2318}";
        let submit_action_label = if will_create { "Create & Add" } else { "Add" };
        let add_shortcut = if has_highlighted_browse_item {
            format!("{submit_modifier} Enter")
        } else {
            "Enter".to_owned()
        };
        let create_shortcut = if has_highlighted_new_project_item {
            format!("{submit_modifier} Enter")
        } else {
            "Enter".to_owned()
        };

        let accessory: Option<AnyElement> = if new_project {
            let creating = self
                .new_project_flow
                .as_ref()
                .is_some_and(|flow| flow.creating);
            let enabled = !self.query.trim().is_empty() && !creating;
            Some(self.submit_button(
                "command-palette-create",
                if creating { "Creating" } else { "Create" },
                create_shortcut.clone(),
                format!("Create ({create_shortcut})"),
                enabled,
                |this, window, cx| this.submit_new_project(window, cx),
                cx,
            ))
        } else if is_browsing {
            Some(self.submit_button(
                "command-palette-add",
                submit_action_label,
                add_shortcut.clone(),
                format!("{submit_action_label} ({add_shortcut})"),
                !relative_blocked,
                |this, window, cx| {
                    let resolved = this.resolved_add_project_path();
                    this.handle_add_project(resolved, window, cx);
                },
                cx,
            ))
        } else {
            None
        };

        let input = div().pr(rems(input_end_padding)).child(
            Input::new(&self.input)
                .appearance(false)
                .bordered(false)
                .p_0()
                .h(rems(2.125)),
        );
        let input_row = command_input_row(start_addon, input, accessory, cx);

        // The project-name preview above the list.
        let preview: Option<AnyElement> = if new_project {
            let snapshot = self.snapshot.as_ref();
            let root = snapshot.and_then(|snapshot| snapshot.new_projects_root.clone());
            root.map(|root| {
                let name = self.query.trim();
                let secondary = if name.is_empty() {
                    format!("Goes in {root}")
                } else {
                    format!(
                        "Creates {}",
                        crate::state::operations::projects::get_new_project_path_preview(
                            &root, name
                        )
                    )
                };
                div()
                    .p_2()
                    .pb_0()
                    .child(
                        div()
                            .flex()
                            .min_h(rems(2.))
                            .items_center()
                            .gap_2()
                            .rounded(RADIUS_SM)
                            .px_2()
                            .py(rems(0.375))
                            .child(Icon::new(IconName::FolderGit2).color(theme.icon_muted))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .min_w_0()
                                    .flex_1()
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(TEXT_SM.size)
                                            .text_color(theme.foreground)
                                            .child(if name.is_empty() {
                                                "New project".to_owned()
                                            } else {
                                                name.to_owned()
                                            }),
                                    )
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(TEXT_XS.size)
                                            .text_color(with_alpha(theme.muted_foreground, 0.85))
                                            .child(secondary),
                                    ),
                            ),
                    )
                    .into_any_element()
            })
        } else {
            None
        };

        // `role="alert"` / `role="status"` lines.
        let status: Option<AnyElement> = if let Some(error) = &self.browse.error {
            Some(status_line(error.clone(), cx))
        } else if self.browse.pending && self.browse.result.is_none() {
            Some(status_line("Loading folders...".to_owned(), cx))
        } else {
            None
        };

        let empty_message: Option<&'static str> = if relative_blocked {
            Some("Relative paths require an active project.")
        } else if will_create {
            Some("Press Enter to create this folder and add it as a project.")
        } else if self.thread_search.pending {
            Some("Searching thread messages…")
        } else {
            None
        };
        self.panel_extra_height =
            if preview.is_some() { 56. } else { 0. } + if status.is_some() { 44. } else { 0. };
        let results = self.render_results(empty_message, window, cx);

        let footer_action_label: Option<&str> = if new_project {
            Some(if self.highlighted.is_none() {
                "Create"
            } else {
                "Select"
            })
        } else if !can_submit_browse_path || has_highlighted_browse_item {
            Some("Select")
        } else {
            None
        };

        let mut footer_left = div()
            .flex()
            .items_center()
            .gap_3()
            .child(command_footer_hint(
                vec![
                    FooterKey::Icon(IconName::ArrowUp),
                    FooterKey::Icon(IconName::ArrowDown),
                ],
                "Navigate",
                cx,
            ));
        if let Some(label) = footer_action_label {
            footer_left = footer_left.child(command_footer_hint(
                vec![FooterKey::Text("Enter".into())],
                label.to_owned(),
                cx,
            ));
        }
        if is_submenu {
            footer_left = footer_left.child(command_footer_hint(
                vec![FooterKey::Text("Backspace".into())],
                "Back",
                cx,
            ));
        }
        footer_left = footer_left.child(command_footer_hint(
            vec![FooterKey::Text("Esc".into())],
            "Close",
            cx,
        ));

        let footer_trailing: Option<AnyElement> = is_browsing.then(|| {
            let picking = self.picking_folder;
            Button::new("command-palette-open-in-finder")
                .variant(ButtonVariant::GhostMuted)
                .size(ButtonSize::Xs)
                .label("Open in Finder")
                .disabled(picking)
                .on_click(cx.listener(|this, _, window, cx| this.pick_folder(window, cx)))
                .into_any_element()
        });

        div()
            .flex()
            .flex_col()
            .min_h_0()
            .child(input_row)
            .child(
                command_panel()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .children(preview)
                    .children(status)
                    .child(results),
            )
            .child(
                command_footer(cx)
                    .child(footer_left)
                    .children(footer_trailing),
            )
            .into_any_element()
    }

    /// The outline `xs` submit button over the input's end
    /// (`absolute inset-e-2.5 top-1/2 -translate-y-1/2`).
    #[allow(clippy::too_many_arguments)]
    fn submit_button(
        &mut self,
        id: &'static str,
        label: &'static str,
        shortcut: String,
        tooltip: String,
        enabled: bool,
        on_click: impl Fn(&mut CommandPalette, &mut Window, &mut Context<CommandPalette>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .right(rems(0.625))
            .flex()
            .items_center()
            .child(
                Button::new(id)
                    .variant(ButtonVariant::Outline)
                    .size(ButtonSize::Xs)
                    .disabled(!enabled)
                    .tooltip(Tooltip::new(tooltip))
                    .label(label)
                    .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
                    .child(
                        // `KbdGroup className="pointer-events-none -me-0.5"`.
                        div()
                            .ml_1()
                            .mr(px(-2.))
                            .child(KbdGroup::new().child(Kbd::new(shortcut))),
                    ),
            )
            .text_color(theme.foreground)
            .into_any_element()
    }
}

fn status_line(message: String, cx: &mut Context<CommandPalette>) -> AnyElement {
    div()
        .px_4()
        .py_3()
        .text_size(TEXT_SM.size)
        .text_color(cx.app_theme().muted_foreground)
        .child(message)
        .into_any_element()
}
