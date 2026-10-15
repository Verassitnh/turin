//! Port of the sidebar parts of upstream/apps/web/src/components/ThreadStatusIndicators.tsx
//! (`ThreadPullRequestBadgeControl`, `ThreadWorktreeIndicator`,
//! `prStatusIndicator`), plus `PULL_REQUEST_STATE_PRESENTATION`
//! (pullRequest/pullRequestPresentation.tsx), `formatWorktreePathForDisplay`
//! (worktreeCleanup.ts) and `formatDiffCount` (client-runtime diffCount.ts).
//!
//! Terminal status (`ThreadTerminalStatusButton`) is cut with the terminal.
//! Stack badges and the PR mini list belong to the PR task (T19); a thread
//! with several visible links shows the `+N` count badge that opens the
//! pull requests surface.

use gpui_kit::{
    App, ClickEvent, CursorStyle, ElementId, Hsla, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, RenderOnce, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, rems,
};

use super::runtime::models::{PullRequestState, ThreadPullRequestLink};
use crate::components::drag_region::no_drag;
use crate::theme::color::{hex, with_alpha};
use crate::theme::tokens::TEXT_XS;
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::sidebar::ClickHandler;
use crate::ui::{Icon, IconName, Tooltip};

/// `formatDiffCount`: the viewer's digit grouping (en-US: `83,065`).
pub fn format_diff_count(count: u64) -> String {
    let digits = count.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// `formatWorktreePathForDisplay`: the last path segment.
pub fn format_worktree_path_for_display(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return path.to_string();
    }
    let normalized = trimmed.replace('\\', "/");
    let normalized = normalized.trim_end_matches('/');
    match normalized.rsplit('/').next().map(str::trim) {
        Some(last) if !last.is_empty() => last.to_string(),
        _ => trimmed.to_string(),
    }
}

/// `PULL_REQUEST_STATE_PRESENTATION[state]`: label, tone and glyph.
pub fn pull_request_state_presentation(
    state: Option<PullRequestState>,
    is_draft: bool,
    theme: &AppTheme,
) -> (&'static str, Hsla, IconName) {
    let dark = theme.is_dark();
    let tone = |light: u32, dark_color: u32, alpha: f32| -> Hsla {
        if dark {
            with_alpha(hex(dark_color), alpha).into()
        } else {
            hex(light).into()
        }
    };
    match (state, is_draft) {
        (Some(PullRequestState::Merged), _) => {
            ("Merged", tone(0x7f22fe, 0xc4b4ff, 0.9), IconName::GitMerge)
        }
        (Some(PullRequestState::Closed), _) => (
            "Closed",
            tone(0xe7000b, 0xffa2a2, 0.9),
            IconName::GitPullRequestClosed,
        ),
        (_, true) => (
            "Draft",
            tone(0x71717b, 0x9f9fa9, 0.8),
            IconName::GitPullRequestDraft,
        ),
        _ => (
            "Open",
            tone(0x009966, 0x5ee9b5, 0.9),
            IconName::GitPullRequestArrow,
        ),
    }
}

/// The linked-PR badge in a row's meta line: state glyph + number, in the
/// state's tone. A single PR opens itself; several open the list.
#[derive(IntoElement)]
pub struct ThreadPullRequestBadge {
    id: ElementId,
    link: ThreadPullRequestLink,
    others: usize,
    on_open_list: Option<ClickHandler>,
}

impl ThreadPullRequestBadge {
    pub fn new(id: impl Into<ElementId>, link: ThreadPullRequestLink, others: usize) -> Self {
        Self {
            id: id.into(),
            link,
            others,
            on_open_list: None,
        }
    }

    pub fn on_open_list(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_open_list = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for ThreadPullRequestBadge {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let (label, tone, icon) =
            pull_request_state_presentation(self.link.state, self.link.is_draft, theme);
        let number = self.link.number;
        let tooltip_lead = format!("PR #{number} - {label}");
        let tooltip: SharedString = match &self.link.title {
            Some(title) => format!("{tooltip_lead}: {title}"),
            None => tooltip_lead,
        }
        .into();
        let (text, tooltip) = if self.others > 0 {
            (
                format!("+{}", self.others + 1),
                SharedString::from(format!(
                    "{tooltip}, and {} more linked; overall {}",
                    self.others,
                    label.to_lowercase()
                )),
            )
        } else {
            (number.to_string(), tooltip)
        };
        let url = self.link.url.clone();
        let opens_list = self.others > 0;
        let on_open_list = self.on_open_list;
        div()
            .id(self.id)
            .on_mouse_down(MouseButton::Left, no_drag)
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap_1()
            .cursor(CursorStyle::PointingHand)
            .text_size(TEXT_XS.size)
            .line_height(TEXT_XS.line_height)
            .text_color(tone)
            .hover(|style| style.opacity(0.8))
            .child(Icon::new(icon).size(rems(0.75)).color(tone))
            .child(text)
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .on_click(move |event, window, cx| {
                cx.stop_propagation();
                if opens_list {
                    if let Some(handler) = &on_open_list {
                        handler(event, window, cx);
                    }
                } else {
                    cx.open_url(&url);
                }
            })
    }
}

/// `ThreadWorktreeIndicator`: a `FolderGit2` glyph for worktree threads.
#[derive(IntoElement)]
pub struct ThreadWorktreeIndicator {
    id: ElementId,
    worktree_path: Option<String>,
    branch: Option<String>,
}

impl ThreadWorktreeIndicator {
    pub fn new(
        id: impl Into<ElementId>,
        worktree_path: Option<String>,
        branch: Option<String>,
    ) -> Self {
        Self {
            id: id.into(),
            worktree_path,
            branch,
        }
    }
}

impl RenderOnce for ThreadWorktreeIndicator {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let path = self
            .worktree_path
            .as_deref()
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(format_worktree_path_for_display);
        let color: Hsla = with_alpha(cx.app_theme().sidebar_scope().muted_foreground, 0.4).into();
        let tooltip: Option<SharedString> = path.map(|path| match &self.branch {
            Some(branch) => format!("Worktree: {path} ({branch})").into(),
            None => format!("Worktree: {path}").into(),
        });
        div()
            .id(self.id)
            .flex_shrink_0()
            .children(tooltip.map(|tooltip| {
                div()
                    .id("worktree")
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        Icon::new(IconName::FolderGit2)
                            .size(rems(0.75))
                            .color(color),
                    )
                    .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_diff_counts_like_intl() {
        assert_eq!(format_diff_count(0), "0");
        assert_eq!(format_diff_count(999), "999");
        assert_eq!(format_diff_count(1240), "1,240");
        assert_eq!(format_diff_count(83065), "83,065");
        assert_eq!(format_diff_count(1234567), "1,234,567");
    }

    #[test]
    fn shows_the_last_worktree_segment() {
        assert_eq!(
            format_worktree_path_for_display("/a/b/T15-sidebar/"),
            "T15-sidebar"
        );
        assert_eq!(format_worktree_path_for_display("C:\\wt\\x"), "x");
        assert_eq!(format_worktree_path_for_display("  "), "  ");
    }
}
