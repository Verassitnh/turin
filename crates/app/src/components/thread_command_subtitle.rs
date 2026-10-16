//! Port of upstream/apps/web/src/components/ThreadCommandSubtitle.tsx
//!
//! The one-line subtitle under a thread in the command palette: project
//! favicon + name, environment, workspace glyph + branch (middle-truncated),
//! the harness icon and a "Current thread" marker, separated by dots.
//!
//! The harness icon is a placeholder glyph until the composer task's
//! `ProviderInstanceIcon` (brand artwork per driver) lands; see Deviations.

use gpui_kit::{
    AnyElement, App, IntoElement, ParentElement as _, RenderOnce, Styled as _, Window, div, rems,
};

use super::command_palette::command_palette_logic::ThreadSubtitle;
use super::project_favicon::ProjectFavicon;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::TEXT_XS;
use crate::ui::icon::{Icon, IconName};
use crate::ui::middle_truncate::MiddleTruncate;

/// Flip this while reviewing command-palette thread subtitles.
/// - `FaviconWorkspaceHarness`: favicon + Folder/FolderGit2 + branch + harness (default)
/// - `FaviconWorkspace`: same without harness
/// - `FaviconBranchHarness`: GitBranch for local, FolderGit2 for worktrees, + harness
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)]
pub enum ThreadCommandSubtitleVariant {
    #[default]
    FaviconWorkspaceHarness,
    FaviconWorkspace,
    FaviconBranchHarness,
}

/// `COMMAND_PALETTE_META_ICON_CLASS`: `size-3 shrink-0 text-muted-foreground/70`.
pub fn command_palette_meta_icon(name: IconName, cx: &App) -> Icon {
    Icon::new(name)
        .size(rems(0.75))
        .color(with_alpha(cx.app_theme().muted_foreground, 0.7).into())
}

/// `CommandPaletteMetaDot`: `shrink-0 text-muted-foreground/50`.
pub fn command_palette_meta_dot(cx: &App) -> AnyElement {
    div()
        .flex_shrink_0()
        .text_color(with_alpha(cx.app_theme().muted_foreground, 0.5))
        .child("·")
        .into_any_element()
}

fn workspace_icon(variant: ThreadCommandSubtitleVariant, is_worktree: bool, cx: &App) -> Icon {
    if is_worktree {
        return command_palette_meta_icon(IconName::FolderGit2, cx);
    }
    if variant == ThreadCommandSubtitleVariant::FaviconBranchHarness {
        return command_palette_meta_icon(IconName::GitBranch, cx);
    }
    command_palette_meta_icon(IconName::Folder, cx)
}

#[derive(IntoElement)]
pub struct ThreadCommandSubtitle {
    pub subtitle: ThreadSubtitle,
    pub favicon: Option<ProjectFavicon>,
    pub variant: ThreadCommandSubtitleVariant,
}

impl ThreadCommandSubtitle {
    pub fn new(subtitle: ThreadSubtitle, favicon: Option<ProjectFavicon>) -> Self {
        Self {
            subtitle,
            favicon,
            variant: ThreadCommandSubtitleVariant::default(),
        }
    }
}

fn non_blank(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

impl RenderOnce for ThreadCommandSubtitle {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let subtitle = self.subtitle;
        let variant = self.variant;
        let is_worktree = non_blank(&subtitle.worktree_path).is_some();
        let show_harness = variant != ThreadCommandSubtitleVariant::FaviconWorkspace
            && subtitle.driver_kind.is_some()
            && subtitle
                .provider_display_name
                .as_deref()
                .is_some_and(|name| !name.is_empty());
        let project_label = non_blank(&subtitle.project_title);
        let branch_label = non_blank(&subtitle.branch);

        let row = div()
            .flex()
            .min_w_0()
            .max_w_full()
            .items_center()
            .gap_1()
            .text_size(TEXT_XS.size)
            .text_color(with_alpha(theme.muted_foreground, 0.7));

        if project_label.is_none()
            && branch_label.is_none()
            && !subtitle.is_current
            && !show_harness
        {
            return row;
        }

        let mut parts: Vec<AnyElement> = Vec::new();
        if let Some(project) = &project_label {
            let mut group = div().flex().min_w_0().items_center().gap_1();
            if let Some(favicon) = self.favicon {
                group = group.child(favicon.size(rems(0.75)));
            }
            group = group.child(div().min_w_0().truncate().child(project.clone()));
            if let Some(label) = non_blank(&subtitle.environment_label) {
                group = group
                    .child(command_palette_meta_dot(cx))
                    .child(div().flex_shrink_0().child(label));
            }
            parts.push(group.into_any_element());
        }

        if let Some(branch) = &branch_label {
            if project_label.is_some() {
                parts.push(command_palette_meta_dot(cx));
            }
            parts.push(
                div()
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap_1()
                    .child(workspace_icon(variant, is_worktree, cx))
                    .child(MiddleTruncate::new(branch.clone()))
                    .into_any_element(),
            );
        }

        if show_harness {
            if project_label.is_some() || branch_label.is_some() {
                parts.push(command_palette_meta_dot(cx));
            }
            // Placeholder harness glyph (`ProviderInstanceIcon`): `size-3 shrink-0 opacity-70`.
            parts.push(
                div()
                    .opacity(0.7)
                    .child(Icon::new(IconName::Bot).size(rems(0.75)))
                    .into_any_element(),
            );
        }

        if subtitle.is_current {
            if project_label.is_some() || branch_label.is_some() || show_harness {
                parts.push(command_palette_meta_dot(cx));
            }
            parts.push(
                div()
                    .flex_shrink_0()
                    .child("Current thread")
                    .into_any_element(),
            );
        }

        row.children(parts)
    }
}
