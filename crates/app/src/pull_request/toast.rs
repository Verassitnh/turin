//! Port of upstream/apps/web/src/components/ui/toast.tsx as the pull request surfaces use it
//! (`toastManager.add({ type, title, description })`): a short stack of cards that each leave
//! after [`TOAST_FOR`].
//!
//! Upstream's manager is one global stack. A surface here owns a [`ToastQueue`] and draws it with
//! [`render_toasts`] over its own bottom edge; the owner is whatever view raised the toast.

use std::time::Duration;

use gpui_kit::{
    AnyElement, App, Context, FontWeight, IntoElement as _, ParentElement as _, SharedString,
    Styled as _, div, px, rems,
};

use super::presentation_view::tone_color;
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{RADIUS_LG, TEXT_SM, TEXT_XS, shadow_md};
use crate::ui::{Icon, IconName};

/// How long a toast stays (the app's `toastManager` default is five seconds).
pub const TOAST_FOR: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Success,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toast {
    pub id: u64,
    pub kind: ToastKind,
    pub title: SharedString,
    pub description: Option<SharedString>,
}

#[derive(Debug, Default)]
pub struct ToastQueue {
    items: Vec<Toast>,
    next_id: u64,
}

impl ToastQueue {
    pub fn items(&self) -> &[Toast] {
        &self.items
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Adds a toast and returns its id; [`Self::dismiss`] takes it away.
    pub fn add(
        &mut self,
        kind: ToastKind,
        title: impl Into<SharedString>,
        description: Option<String>,
    ) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.items.push(Toast {
            id,
            kind,
            title: title.into(),
            description: description.map(Into::into),
        });
        id
    }

    pub fn dismiss(&mut self, id: u64) {
        self.items.retain(|toast| toast.id != id);
    }
}

impl ToastQueue {
    /// `toastManager.add` from a view that holds this queue (`queue_of` finds it again once the
    /// toast's time is up).
    pub fn push<T: 'static>(
        &mut self,
        cx: &mut Context<T>,
        queue_of: fn(&mut T) -> &mut ToastQueue,
        kind: ToastKind,
        title: impl Into<SharedString>,
        description: Option<String>,
    ) {
        let id = self.add(kind, title, description);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TOAST_FOR).await;
            this.update(cx, |this, cx| {
                queue_of(this).dismiss(id);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
}

/// The stack, laid over the bottom of whatever surface owns the queue.
pub fn render_toasts(queue: &ToastQueue, cx: &App) -> Option<AnyElement> {
    if queue.is_empty() {
        return None;
    }
    let theme = cx.app_theme();
    Some(
        div()
            .absolute()
            .bottom(rems(0.75))
            .left(rems(1.))
            .right(rems(1.))
            .flex()
            .flex_col()
            .gap_2()
            .children(queue.items().iter().map(|toast| {
                let (icon, color) = match toast.kind {
                    ToastKind::Success => (
                        IconName::CircleCheck,
                        tone_color("text-emerald-600 dark:text-emerald-300/90", theme),
                    ),
                    ToastKind::Error => (IconName::CircleAlert, theme.destructive),
                };
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .rounded(RADIUS_LG)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.popover)
                    .shadow(shadow_md(0.05))
                    .px_3()
                    .py_2()
                    .child(Icon::new(icon).size(rems(1.)).color(color))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w_0()
                            .gap(px(2.))
                            .child(
                                div()
                                    .text_size(TEXT_SM.size)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.foreground)
                                    .child(toast.title.clone()),
                            )
                            .children(toast.description.clone().map(|description| {
                                div()
                                    .text_size(TEXT_XS.size)
                                    .text_color(theme.muted_foreground)
                                    .child(description)
                            })),
                    )
            }))
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dismiss_removes_only_the_named_toast() {
        let mut queue = ToastQueue::default();
        let first = queue.add(ToastKind::Success, "one", None);
        let second = queue.add(ToastKind::Error, "two", Some("why".into()));
        assert_ne!(first, second);
        queue.dismiss(first);
        assert_eq!(queue.items().len(), 1);
        assert_eq!(queue.items()[0].id, second);
        assert_eq!(queue.items()[0].description.as_deref(), Some("why"));
        queue.dismiss(second);
        assert!(queue.is_empty());
    }
}
