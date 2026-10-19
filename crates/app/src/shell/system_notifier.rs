//! The desktop half of upstream's `new Notification(title, { body, tag, silent })`
//! with its `click` listener, on the operating system's notification center.
//!
//! [`install`] makes a [`DesktopNotifier`] of it for the whole app: thread
//! notices and pull request guide notices both go through
//! [`show_desktop_notification`]. A notification only appears while the
//! client setting `notificationMode` shows desktop notifications
//! (`hasDesktopNotifications`); a click brings the app forward and runs the
//! notification's own action.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::{App, Global, SystemNotification};
use turin_contracts::settings::NotificationMode;

use crate::pull_request::desktop_notification::{
    DesktopNotification, DesktopNotifier, set_desktop_notifier,
};
use crate::shell::thread_notifications::has_desktop_notifications;

type ClickHandler = Rc<dyn Fn(&mut App)>;

/// The client setting `notificationMode`, kept current by the shell.
struct ModeGlobal(NotificationMode);

impl Global for ModeGlobal {}

/// Tells the notifier which `notificationMode` is in force.
pub fn set_notification_mode(mode: NotificationMode, cx: &mut App) {
    cx.set_global(ModeGlobal(mode));
}

fn shows_notifications(cx: &App) -> bool {
    cx.try_global::<ModeGlobal>()
        .is_some_and(|mode| has_desktop_notifications(mode.0))
}

struct SystemNotifier {
    /// Click handlers by tag; a later notification with the tag replaces its.
    handlers: Rc<RefCell<HashMap<String, ClickHandler>>>,
}

impl DesktopNotifier for SystemNotifier {
    fn show(&self, notification: DesktopNotification, cx: &mut App) {
        if !shows_notifications(cx) {
            return;
        }
        match notification.on_click {
            Some(handler) => {
                self.handlers
                    .borrow_mut()
                    .insert(notification.tag.clone(), handler);
            }
            None => {
                self.handlers.borrow_mut().remove(&notification.tag);
            }
        }
        cx.show_system_notification(SystemNotification {
            tag: notification.tag.into(),
            title: notification.title.into(),
            body: notification.body.into(),
            actions: Vec::new(),
        });
    }
}

/// Raises desktop notifications through the system and routes their clicks.
pub fn install(cx: &mut App) {
    let handlers: Rc<RefCell<HashMap<String, ClickHandler>>> = Rc::default();
    set_desktop_notifier(
        Rc::new(SystemNotifier {
            handlers: handlers.clone(),
        }),
        cx,
    );
    cx.on_system_notification_response(move |response, cx| {
        let handler = handlers.borrow().get(response.tag.as_ref()).cloned();
        cx.activate(true);
        if let Some(handler) = handler {
            handler(cx);
        }
    });
}
