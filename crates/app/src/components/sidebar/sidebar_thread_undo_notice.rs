//! Port of upstream `SidebarThreadUndoNotice.tsx`.
//!
//! Port of upstream/apps/web/src/components/sidebar/SidebarThreadUndoNotice.tsx.

use gpui_kit::{App, IntoElement, ParentElement, RenderOnce, Styled, WeakEntity, Window, div};

use crate::components::sidebar::Sidebar;
use turin_shared::keybindings::is_mac_platform;

use crate::components::command_palette::shortcut_labels::default_shortcut_label_for_command;

use crate::hooks::thread_undo::UndoAction;
use crate::shell::composer_draft_host::ComposerDraftHostGlobal;
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{CONTROL_RADIUS, TEXT_SM};
use crate::ui::{Button, ButtonSize, ButtonVariant};

#[derive(IntoElement)]
pub struct SidebarThreadUndoNotice {
    sidebar: WeakEntity<Sidebar>,
}

impl SidebarThreadUndoNotice {
    pub fn new(sidebar: WeakEntity<Sidebar>) -> Self {
        Self { sidebar }
    }
}

impl RenderOnce for SidebarThreadUndoNotice {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let Some(global) = cx.try_global::<ComposerDraftHostGlobal>() else {
            return div();
        };
        let Some(notice) = global.0.read(cx).current_notice() else {
            return div();
        };
        let theme = cx.app_theme();
        let noun = if notice.action == UndoAction::Discarded {
            if notice.count == 1 { "draft" } else { "drafts" }
        } else if notice.count == 1 {
            "thread"
        } else {
            "threads"
        };
        let mac = is_mac_platform(if cfg!(target_os = "macos") {
            "MacIntel"
        } else {
            "Linux x86_64"
        });
        let label = default_shortcut_label_for_command("thread.undo", mac)
            .map(|label| format!("{label} to undo"))
            .unwrap_or_else(|| "Undo".to_owned());
        let sidebar = self.sidebar;
        div()
            .mx_2()
            .mb_2()
            .rounded(CONTROL_RADIUS)
            .border_1()
            .border_color(theme.border)
            .bg(theme.muted)
            .px_3()
            .py_2()
            .text_size(TEXT_SM.size)
            .child(format!(
                "{} {} {}, ",
                notice.action.label(),
                notice.count,
                noun
            ))
            .child(
                Button::new("sidebar-thread-undo")
                    .size(ButtonSize::Xs)
                    .variant(ButtonVariant::Ghost)
                    .label(label)
                    .on_click(move |_, window, cx| {
                        sidebar
                            .update(cx, |sidebar, cx| sidebar.undo_latest(window, cx))
                            .ok();
                    }),
            )
    }
}
