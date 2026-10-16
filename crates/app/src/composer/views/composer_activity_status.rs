//! Port of upstream/apps/web/src/components/chat/ComposerActivityStatus.tsx
//!
//! The row the composer shows while a thread's messages load or sync: a
//! spinner in the banner icon column and the phase label from
//! `threadSyncLabel`. It sits in the banner stack as the activity entry, or
//! alone in the activity strip.

use gpui_kit::{AnyElement, IntoElement as _, ParentElement as _, Styled as _, div};

use crate::composer::views::composer_banner::banner_row;
use crate::state::thread_sync::{ThreadSyncPhase, thread_sync_label};
use crate::theme::AppTheme;
use crate::ui::spinner::{Spinner, SpinnerSize};

/// `ComposerActivityRow`: `shrink-0 whitespace-nowrap text-muted-foreground`
/// beside a 12px spinner (`ComposerBanner.Icon`'s `[&>svg]:size-3`).
pub fn composer_activity_row(phase: ThreadSyncPhase, theme: &AppTheme) -> AnyElement {
    banner_row(
        Some(
            Spinner::new("composer-sync-spinner")
                .size(SpinnerSize::Xs)
                .color(theme.muted_foreground)
                .into_any_element(),
        ),
        div()
            .flex_shrink_0()
            .whitespace_nowrap()
            .text_color(theme.muted_foreground)
            .child(thread_sync_label(phase))
            .into_any_element(),
        Vec::new(),
    )
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_row_reads_the_phase_label() {
        assert_eq!(
            thread_sync_label(ThreadSyncPhase::Loading),
            "Loading messages..."
        );
        assert_eq!(
            thread_sync_label(ThreadSyncPhase::Syncing),
            "Syncing messages..."
        );
    }
}
