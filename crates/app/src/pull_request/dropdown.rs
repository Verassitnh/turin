//! A menu opened from one of our `Button`s.
//!
//! gpui-component's `DropdownMenu` extension only attaches to its own button type, but the
//! popup is built from public parts: a `Popover` anchored to the trigger whose content is a
//! `PopupMenu`. This is the same wiring (build the menu once per open, drop it on dismiss) for
//! any trigger that is `Selectable`, which our `Button` is.

use gpui_kit::component::Selectable;
use gpui_kit::component::menu::PopupMenu;
use gpui_kit::component::popover::Popover;
use gpui_kit::{
    Anchor, App, Context, DismissEvent, ElementId, Entity, Focusable as _, IntoElement,
    SharedString, Window,
};

#[derive(Default)]
struct DropdownState {
    menu: Option<Entity<PopupMenu>>,
}

/// `<Menu><MenuTrigger render={trigger}/><MenuPopup>…</MenuPopup></Menu>`.
pub fn dropdown<T>(
    id: impl Into<ElementId>,
    anchor: Anchor,
    trigger: T,
    build: impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
) -> Popover
where
    T: Selectable + IntoElement + 'static,
{
    let id: ElementId = id.into();
    let popover_id: SharedString = format!("dropdown:{id:?}").into();
    let build = std::rc::Rc::new(build);
    Popover::new(popover_id.clone())
        .appearance(false)
        .default_open(demo_open(&popover_id))
        .overlay_closable(false)
        .anchor(anchor)
        .trigger(trigger)
        .content(move |_, window, cx: &mut Context<_>| {
            let state = window.use_keyed_state(popover_id.clone(), cx, |_, _| DropdownState::default());
            if let Some(menu) = state.read(cx).menu.clone() {
                return menu;
            }
            let build = build.clone();
            let menu = PopupMenu::build(window, cx, move |menu, window, cx| build(menu, window, cx));
            menu.focus_handle(cx).focus(window, cx);
            let popover_state = cx.entity().downgrade();
            let weak_state = state.downgrade();
            window
                .subscribe(&menu, cx, move |_, _: &DismissEvent, window, cx| {
                    if let Some(popover_state) = popover_state.upgrade() {
                        popover_state.update(cx, |popover, cx| popover.dismiss(window, cx));
                    }
                    weak_state.update(cx, |state, _| state.menu = None).ok();
                })
                .detach();
            state.update(cx, |state, _| state.menu = Some(menu.clone()));
            menu
        })
}

/// Dev hook for screenshots: `TURIN_PR_OPEN=<id>` opens the menu or popover whose id contains it.
pub fn demo_open(id: &str) -> bool {
    std::env::var("TURIN_PR_OPEN").is_ok_and(|wanted| !wanted.is_empty() && id.contains(&wanted))
}

#[allow(dead_code)]
fn _unused(_: &App) {}
