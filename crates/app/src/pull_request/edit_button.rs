//! Port of upstream/apps/web/src/components/pullRequest/PullRequestEditButton.tsx: the pencil that
//! reveals on hovering its row (`group-hover:opacity-100`; the touch rule is not needed here).

use gpui_kit::{
    App, ClickEvent, ElementId, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, SharedString, Styled as _, Window, div, rems,
};
use std::rc::Rc;

use crate::ui::{Button, ButtonSize, ButtonVariant, IconName};

#[derive(IntoElement)]
pub struct PullRequestEditButton {
    id: ElementId,
    /// The `group` the row declares; the pencil shows while it is hovered.
    group: SharedString,
    label: SharedString,
    on_click: Option<Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>>,
}

impl PullRequestEditButton {
    pub fn new(
        id: impl Into<ElementId>,
        group: impl Into<SharedString>,
        label: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            group: group.into(),
            label: label.into(),
            on_click: None,
        }
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for PullRequestEditButton {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let on_click = self.on_click;
        let mut button = Button::new(self.id)
            .size(ButtonSize::IconXs)
            .variant(ButtonVariant::GhostMuted)
            .icon(IconName::Pencil)
            .icon_size(rems(0.75))
            .tooltip(self.label);
        if let Some(on_click) = on_click {
            button = button.on_click(move |event, window, cx| on_click(event, window, cx));
        }
        div()
            .flex()
            .flex_shrink_0()
            .opacity(0.)
            .group_hover(self.group, |style| style.opacity(1.))
            .child(button)
    }
}
