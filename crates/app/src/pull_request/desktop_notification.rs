//! The desktop notification half of upstream/apps/web/src/components/pullRequest/guide/
//! PullRequestGuideRunCoordinator.tsx (`new Notification(title, { body, tag, silent: true })`).
//!
//! The app has no notification service yet (upstream's `threadNotifications` is not ported), so
//! this is the seam one will plug into: a [`DesktopNotifier`] set as a global. Until one is set a
//! notification is logged, which is all a window that is not on screen can be told.

use std::rc::Rc;

use gpui_kit::{App, Global};

/// What `new Notification(title, { body, tag })` takes, plus what a click does.
#[derive(Clone)]
pub struct DesktopNotification {
    pub title: String,
    pub body: String,
    /// A later notification with the same tag replaces this one.
    pub tag: String,
    /// The click: bring the app forward and open what the notification is about.
    pub on_click: Option<Rc<dyn Fn(&mut App)>>,
}

pub trait DesktopNotifier {
    fn show(&self, notification: DesktopNotification, cx: &mut App);
}

struct NotifierGlobal(Rc<dyn DesktopNotifier>);

impl Global for NotifierGlobal {}

/// Makes `notifier` the app's way to raise desktop notifications.
pub fn set_desktop_notifier(notifier: Rc<dyn DesktopNotifier>, cx: &mut App) {
    cx.set_global(NotifierGlobal(notifier));
}

/// Raises a desktop notification through the app's notifier, or logs it when there is none.
pub fn show_desktop_notification(notification: DesktopNotification, cx: &mut App) {
    match cx
        .try_global::<NotifierGlobal>()
        .map(|global| global.0.clone())
    {
        Some(notifier) => notifier.show(notification, cx),
        None => tracing::info!(
            title = %notification.title,
            body = %notification.body,
            tag = %notification.tag,
            "desktop notification (no notifier installed)"
        ),
    }
}
