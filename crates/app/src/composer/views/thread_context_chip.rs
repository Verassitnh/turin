//! Port of upstream/apps/web/src/components/ThreadContextChip.tsx
//!
//! Inline chip for an attached thread, in the composer and in sent messages.
//! Prefers the live title so a renamed thread never shows a stale label, and
//! opens the thread on click. The router `Link` is `on_open`; the live shell
//! lookup (`useThreadShell`) is the caller's `shell_title`.

use gpui_kit::{App, IntoElement, RenderOnce, SharedString, Window};

use crate::composer::views::composer_banner::WindowCallback;
use crate::composer::views::context_chip::{ContextChip, ContextChipKind};
use crate::ui::IconName;

/// The shell the chip's thread resolves to, when it still exists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadShellTitle {
    pub title: Option<String>,
}

/// `shell?.title?.trim() || record.title`.
pub fn thread_chip_title(shell: Option<&ThreadShellTitle>, record_title: &str) -> String {
    shell
        .and_then(|shell| shell.title.as_deref())
        .map(turin_contracts::base_schemas::js_trim)
        .filter(|title| !title.is_empty())
        .unwrap_or(record_title)
        .to_owned()
}

/// The tooltip: `shell ? "Open thread" : "Thread no longer available"`.
pub fn thread_chip_tooltip(shell: Option<&ThreadShellTitle>) -> &'static str {
    if shell.is_some() {
        "Open thread"
    } else {
        "Thread no longer available"
    }
}

/// `aria-label`.
pub fn thread_chip_aria_label(title: &str) -> String {
    format!("Thread, {title}")
}

#[derive(IntoElement)]
pub struct ThreadContextChip {
    /// `record.title`, the title when the thread was attached.
    pub record_title: String,
    pub shell: Option<ThreadShellTitle>,
    pub on_open: WindowCallback,
    pub id: SharedString,
}

impl RenderOnce for ThreadContextChip {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let title = thread_chip_title(self.shell.as_ref(), &self.record_title);
        let on_open = self.on_open;
        ContextChip::new(self.id)
            .kind(ContextChipKind::Thread)
            .icon(IconName::MessagesSquare)
            .label(title)
            .tooltip(thread_chip_tooltip(self.shell.as_ref()))
            .on_click(move |_, window, cx| on_open(window, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_the_live_title_over_the_recorded_one() {
        let shell = ThreadShellTitle {
            title: Some("  Renamed thread ".into()),
        };
        assert_eq!(
            thread_chip_title(Some(&shell), "Old title"),
            "Renamed thread"
        );
    }

    #[test]
    fn falls_back_to_the_recorded_title_for_a_blank_or_missing_shell() {
        let blank = ThreadShellTitle {
            title: Some("   ".into()),
        };
        assert_eq!(thread_chip_title(Some(&blank), "Old title"), "Old title");
        assert_eq!(thread_chip_title(None, "Old title"), "Old title");
    }

    #[test]
    fn says_when_the_thread_is_gone() {
        assert_eq!(thread_chip_tooltip(None), "Thread no longer available");
        assert_eq!(
            thread_chip_tooltip(Some(&ThreadShellTitle { title: None })),
            "Open thread"
        );
        assert_eq!(thread_chip_aria_label("Plan"), "Thread, Plan");
    }
}
