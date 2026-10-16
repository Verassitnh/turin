//! Port of upstream/apps/web/src/components/chat/ThreadContextDragGhost.tsx
//!
//! Follows the pointer while a sidebar thread is dragged toward a composer.
//! The portal to `document.body` is an absolutely positioned child of the
//! window overlay layer; `translate(x + 12px, y + 12px)` is its offset.

// The sidebar does not start thread-to-composer drags yet, so nothing
// paints the ghost (upstream mounts it in `Sidebar.tsx`).
#![allow(dead_code)]

use gpui_kit::{
    AnyElement, FontWeight, IntoElement as _, ParentElement as _, Styled as _, div, px, rems,
};

use super::thread_context_drag::ThreadContextDragGhost;
use crate::theme::AppTheme;
use crate::theme::tokens::{self, RADIUS_LG, TEXT_SM, TEXT_XS};
use crate::ui::{Icon, IconName};

/// The pointer offset of the ghost's top-left corner.
pub const THREAD_CONTEXT_DRAG_GHOST_OFFSET_PX: f32 = 12.;

/// `+N` for the threads beyond the titled one; nothing for one thread.
pub fn thread_context_drag_extra_count(count: usize) -> Option<String> {
    (count > 1).then(|| format!("+{}", count - 1))
}

/// `ThreadContextDragGhost`; `None` while nothing is dragged.
pub fn thread_context_drag_ghost(
    ghost: Option<&ThreadContextDragGhost>,
    theme: &AppTheme,
) -> Option<AnyElement> {
    let ghost = ghost?;
    let extra = thread_context_drag_extra_count(ghost.count);
    Some(
        div()
            .absolute()
            .left(px(ghost.x as f32 + THREAD_CONTEXT_DRAG_GHOST_OFFSET_PX))
            .top(px(ghost.y as f32 + THREAD_CONTEXT_DRAG_GHOST_OFFSET_PX))
            .flex()
            .max_w(rems(18.))
            .items_center()
            .gap_2()
            .rounded(RADIUS_LG)
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .px_3()
            .py_2()
            .text_size(TEXT_SM.size)
            .text_color(theme.popover_foreground)
            .shadow(tokens::shadow_lg(0.1))
            .child(
                Icon::new(IconName::MessagesSquare)
                    .size(rems(1.))
                    .color(theme.secondary_label),
            )
            .child(div().min_w_0().truncate().child(ghost.title.clone()))
            .children(extra.map(|extra| {
                div()
                    .flex_shrink_0()
                    .rounded_full()
                    .bg(theme.muted)
                    .px(rems(0.375))
                    .text_size(TEXT_XS.size)
                    .font_weight(FontWeight::NORMAL)
                    .text_color(theme.secondary_label)
                    .child(extra)
            }))
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_badge_counts_the_threads_beyond_the_first() {
        assert_eq!(thread_context_drag_extra_count(1), None);
        assert_eq!(thread_context_drag_extra_count(2).as_deref(), Some("+1"));
        assert_eq!(thread_context_drag_extra_count(5).as_deref(), Some("+4"));
    }
}
