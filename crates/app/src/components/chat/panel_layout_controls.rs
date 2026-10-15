//! Port of upstream/apps/web/src/components/chat/PanelLayoutControls.tsx
//!
//! The fixed titlebar toggles at the top right: thread details, right panel,
//! and maximize while the right panel is open. Each is `Toggle variant=ghost
//! size=sm` (28px, `rounded-lg`, pressed `bg-accent`, icon `size-4`). The
//! terminal drawer toggle is cut with the terminal.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{App, ClickEvent, IntoElement, ParentElement as _, RenderOnce, Styled as _, Window, div};

use crate::theme::tokens::RADIUS_LG;
use crate::ui::{Button, ButtonSize, ButtonVariant, IconName, Tooltip};

type Handler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct PanelLayoutControls {
    pub right_panel_open: bool,
    pub right_panel_maximized: bool,
    pub on_toggle_right_panel: Handler,
    pub on_toggle_maximize: Handler,
}

fn toggle(id: &'static str, icon: IconName, pressed: bool, tooltip: Tooltip) -> Button {
    Button::new(id)
        .variant(ButtonVariant::Ghost)
        .size(ButtonSize::IconSm)
        .icon(icon)
        .pressed(pressed)
        .tooltip(tooltip)
        .rounded(RADIUS_LG)
}

impl RenderOnce for PanelLayoutControls {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let on_toggle_right_panel = self.on_toggle_right_panel;
        let on_toggle_maximize = self.on_toggle_maximize;
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(toggle(
                "toggle-thread-details",
                IconName::SquareMenu,
                false,
                Tooltip::new("Thread details").shortcut("⇧⌘B"),
            ))
            .child(
                toggle(
                    "toggle-right-panel",
                    IconName::PanelRight,
                    self.right_panel_open,
                    Tooltip::new("Toggle panel").shortcut("⌥⌘B"),
                )
                .on_click(move |event, window, cx| on_toggle_right_panel(event, window, cx)),
            )
            .when(self.right_panel_open, |this| {
                this.child(
                    toggle(
                        "toggle-right-panel-maximize",
                        if self.right_panel_maximized {
                            IconName::Minimize2
                        } else {
                            IconName::Maximize2
                        },
                        self.right_panel_maximized,
                        Tooltip::new(if self.right_panel_maximized {
                            "Restore panel"
                        } else {
                            "Maximize panel"
                        }),
                    )
                    .on_click(move |event, window, cx| on_toggle_maximize(event, window, cx)),
                )
            })
    }
}
