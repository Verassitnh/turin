//! Port of upstream/apps/web/src/components/preview/PreviewPanelShell.tsx
//!
//! The shell around a right-column panel: in `inline` mode it is a
//! `border-l` column whose width the owner resolves (the user-resizable width
//! comes from `hooks::use_preview_panel_inline_size`, the drag handle is
//! `AppSidebarLayout`'s), `maximized` makes it fill the row, and an `open`
//! value makes it collapsible so it animates between zero and its width. In
//! `sheet`, `sidebar` and `embedded` modes the parent owns the size
//! (`w-full`).
//!
//! Upstream re-exports `getPreviewPanelMaxWidth` from this file; here it lives
//! in `hooks::use_preview_panel_inline_size::get_preview_panel_max_width`,
//! next to its ported tests.
//!
//! The browser preview is cut, but the pull requests page shares this shell.
//! `widthStorageKey` and `defaultWidth` belong to
//! `usePreviewPanelInlineSize`, which the owner of the panel drives.
//!
//! Not ported: the Electron `useDragRegion` flag (it is read nowhere in the
//! upstream body), CSS transitions and `starting:` styles (GPUI has no CSS
//! transitions, so the width changes in one frame, and
//! [`PreviewPanelLayoutTransition`] keeps the suppression state machine for an
//! owner that animates it), and `pointer-events-none` on a closed panel (it
//! is clipped to zero width). See PORTING.md Deviations.


use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, IntoElement, ParentElement, RenderOnce, Styled, Window, div, px, relative,
};

use crate::theme::ActiveAppTheme;

/// `PreviewPanelMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewPanelMode {
    Inline,
    Sheet,
    Sidebar,
    Embedded,
}

impl PreviewPanelMode {
    /// The `data-preview-panel-mode` attribute value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::Sheet => "sheet",
            Self::Sidebar => "sidebar",
            Self::Embedded => "embedded",
        }
    }
}

/// The derived transition state in `PreviewPanelShellFrame`: the last seen
/// `open`/`width`/`maximized`, and whether the width transition is
/// suppressed. Suppression is derived before the layout commits so a resize
/// or maximize change never creates a width transition; it ends after two
/// painted frames (upstream's double `requestAnimationFrame`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreviewPanelLayoutTransition {
    pub open: bool,
    pub width: f32,
    pub maximized: bool,
    pub suppressed: bool,
    frames_painted: u8,
}

impl PreviewPanelLayoutTransition {
    pub fn new(open: bool, width: f32, maximized: bool) -> Self {
        Self {
            open,
            width,
            maximized,
            suppressed: false,
            frames_painted: 0,
        }
    }

    /// Called on every render with the current values. Returns whether the
    /// state changed.
    pub fn observe(&mut self, collapsible: bool, open: bool, width: f32, maximized: bool) -> bool {
        if self.open == open && self.width == width && self.maximized == maximized {
            return false;
        }
        let suppressed = collapsible
            && self.open == open
            && (self.width != width || self.maximized != maximized);
        *self = Self {
            open,
            width,
            maximized,
            suppressed,
            frames_painted: 0,
        };
        true
    }

    /// A frame painted while suppressed. Returns whether suppression just
    /// ended (`setLayoutTransition({ ...current, suppressed: false })`).
    pub fn on_frame_painted(&mut self) -> bool {
        if !self.suppressed {
            return false;
        }
        self.frames_painted += 1;
        if self.frames_painted < 2 {
            return false;
        }
        self.suppressed = false;
        self.frames_painted = 0;
        true
    }
}

/// How wide the shell's host is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PreviewPanelHostWidth {
    /// Not inline: `w-full`.
    Fill,
    /// Maximized inline: `width: 100%`.
    Full,
    /// `width: var(--resizable-width)` or `0px` while closed.
    Px(f32),
}

/// The geometry the shell's classes and inline styles compute to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreviewPanelShellGeometry {
    pub inline: bool,
    pub collapsible: bool,
    pub open: bool,
    pub maximized: bool,
    pub host_width: PreviewPanelHostWidth,
    /// Inline: `flex-1` when maximized, `shrink-0` otherwise.
    pub fills_row: bool,
    /// `border-l border-border`, inline only.
    pub border_left: bool,
    /// The drag handle shows for an inline panel that is not maximized.
    pub show_resize_handle: bool,
    /// `overflow-clip` on the content wrapper.
    pub clip_overflow: bool,
    /// `calc(var(--resizable-width) - 1px)` on the inner column.
    pub content_width: Option<f32>,
}

/// `PreviewPanelShellFrame`'s derived flags and widths.
pub fn preview_panel_shell_geometry(
    mode: PreviewPanelMode,
    maximized: bool,
    open: Option<bool>,
    width: f32,
) -> PreviewPanelShellGeometry {
    let inline = mode == PreviewPanelMode::Inline;
    let collapsible = inline && open.is_some();
    let open = open.unwrap_or(true);
    let host_width = if !inline {
        PreviewPanelHostWidth::Fill
    } else if maximized {
        PreviewPanelHostWidth::Full
    } else if collapsible && !open {
        PreviewPanelHostWidth::Px(0.)
    } else {
        PreviewPanelHostWidth::Px(width)
    };
    PreviewPanelShellGeometry {
        inline,
        collapsible,
        open,
        maximized,
        host_width,
        fills_row: inline && maximized,
        border_left: inline,
        show_resize_handle: inline && !maximized,
        clip_overflow: collapsible,
        content_width: (collapsible && !maximized).then_some(width - 1.),
    }
}

/// `PreviewPanelShell`.
#[derive(IntoElement)]
pub struct PreviewPanelShell {
    mode: PreviewPanelMode,
    maximized: bool,
    open: Option<bool>,
    width: f32,
    resize_handle: Option<AnyElement>,
    children: Vec<AnyElement>,
}

impl PreviewPanelShell {
    /// `width` is the resolved inline width in px (`inlineSize.width`).
    pub fn new(mode: PreviewPanelMode, width: f32) -> Self {
        Self {
            mode,
            maximized: false,
            open: None,
            width,
            resize_handle: None,
            children: Vec::new(),
        }
    }

    pub fn maximized(mut self, maximized: bool) -> Self {
        self.maximized = maximized;
        self
    }

    /// Makes an inline panel collapsible; `false` collapses it to zero width.
    pub fn open(mut self, open: bool) -> Self {
        self.open = Some(open);
        self
    }

    /// The `RightPanelResizeHandle`, drawn for an inline panel that is not
    /// maximized.
    pub fn resize_handle(mut self, handle: impl IntoElement) -> Self {
        self.resize_handle = Some(handle.into_any_element());
        self
    }
}

impl ParentElement for PreviewPanelShell {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for PreviewPanelShell {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let geometry =
            preview_panel_shell_geometry(self.mode, self.maximized, self.open, self.width);

        // `relative flex h-full min-h-0 min-w-0 max-w-full flex-col bg-background`.
        let host = div()
            .relative()
            .flex()
            .flex_col()
            .h_full()
            .min_h_0()
            .min_w_0()
            .max_w_full()
            .bg(theme.background)
            .when(geometry.fills_row, |this| this.flex_1())
            .when(geometry.inline && !geometry.fills_row, |this| {
                this.flex_shrink_0()
            })
            .when(geometry.border_left, |this| {
                this.border_l_1().border_color(theme.border)
            })
            .map(|this| match geometry.host_width {
                PreviewPanelHostWidth::Fill => this.w_full(),
                PreviewPanelHostWidth::Full => this.w(relative(1.)),
                PreviewPanelHostWidth::Px(width) => this.w(px(width)),
            });

        // `h-full min-h-0 w-full`, `overflow-clip` while collapsible.
        let content = div()
            .h_full()
            .min_h_0()
            .w_full()
            .when(geometry.clip_overflow, |this| this.overflow_hidden())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .h_full()
                    .min_h_0()
                    .min_w_0()
                    .when_some(geometry.content_width, |this, width| this.w(px(width)))
                    .children(self.children),
            );

        host.children(self.resize_handle.filter(|_| geometry.show_resize_handle))
            .child(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_attribute_values() {
        assert_eq!(PreviewPanelMode::Inline.as_str(), "inline");
        assert_eq!(PreviewPanelMode::Sheet.as_str(), "sheet");
        assert_eq!(PreviewPanelMode::Sidebar.as_str(), "sidebar");
        assert_eq!(PreviewPanelMode::Embedded.as_str(), "embedded");
    }

    #[test]
    fn inline_panel_uses_the_resolved_width_with_a_border_and_handle() {
        let geometry = preview_panel_shell_geometry(PreviewPanelMode::Inline, false, None, 540.);
        assert_eq!(geometry.host_width, PreviewPanelHostWidth::Px(540.));
        assert!(geometry.border_left);
        assert!(geometry.show_resize_handle);
        assert!(!geometry.fills_row);
        assert!(!geometry.collapsible);
        assert!(!geometry.clip_overflow);
        assert_eq!(geometry.content_width, None);
    }

    #[test]
    fn maximized_inline_panel_fills_the_row_without_a_handle() {
        let geometry =
            preview_panel_shell_geometry(PreviewPanelMode::Inline, true, Some(true), 540.);
        assert_eq!(geometry.host_width, PreviewPanelHostWidth::Full);
        assert!(geometry.fills_row);
        assert!(!geometry.show_resize_handle);
        assert!(geometry.border_left);
        // Maximized keeps the content unconstrained.
        assert_eq!(geometry.content_width, None);
    }

    #[test]
    fn collapsible_panel_closes_to_zero_width_and_clips() {
        let closed =
            preview_panel_shell_geometry(PreviewPanelMode::Inline, false, Some(false), 540.);
        assert_eq!(closed.host_width, PreviewPanelHostWidth::Px(0.));
        assert!(closed.collapsible);
        assert!(!closed.open);
        assert!(closed.clip_overflow);
        // The inner column keeps its width minus the border so it clips, not reflows.
        assert_eq!(closed.content_width, Some(539.));

        let open = preview_panel_shell_geometry(PreviewPanelMode::Inline, false, Some(true), 540.);
        assert_eq!(open.host_width, PreviewPanelHostWidth::Px(540.));
        assert_eq!(open.content_width, Some(539.));
    }

    #[test]
    fn maximized_wins_over_closed() {
        let geometry =
            preview_panel_shell_geometry(PreviewPanelMode::Inline, true, Some(false), 540.);
        assert_eq!(geometry.host_width, PreviewPanelHostWidth::Full);
    }

    #[test]
    fn other_modes_are_full_width_and_never_collapsible() {
        for mode in [
            PreviewPanelMode::Sheet,
            PreviewPanelMode::Sidebar,
            PreviewPanelMode::Embedded,
        ] {
            let geometry = preview_panel_shell_geometry(mode, true, Some(false), 540.);
            assert_eq!(geometry.host_width, PreviewPanelHostWidth::Fill);
            assert!(!geometry.border_left);
            assert!(!geometry.collapsible);
            assert!(!geometry.show_resize_handle);
        }
    }

    #[test]
    fn open_defaults_to_true_when_not_provided() {
        assert!(preview_panel_shell_geometry(PreviewPanelMode::Inline, false, None, 400.).open);
    }

    #[test]
    fn width_or_maximize_change_suppresses_the_transition_when_collapsible() {
        let mut transition = PreviewPanelLayoutTransition::new(true, 540., false);
        assert!(!transition.observe(true, true, 540., false));
        assert!(!transition.suppressed);

        assert!(transition.observe(true, true, 600., false));
        assert!(transition.suppressed);
        assert_eq!(transition.width, 600.);

        // Painting two frames restores the transition.
        assert!(!transition.on_frame_painted());
        assert!(transition.suppressed);
        assert!(transition.on_frame_painted());
        assert!(!transition.suppressed);

        assert!(transition.observe(true, true, 600., true));
        assert!(transition.suppressed);
    }

    #[test]
    fn open_change_keeps_the_transition_and_non_collapsible_never_suppresses() {
        let mut transition = PreviewPanelLayoutTransition::new(true, 540., false);
        assert!(transition.observe(true, false, 540., false));
        assert!(!transition.suppressed);

        // Open and width change together: the open animation wins.
        assert!(transition.observe(true, true, 700., false));
        assert!(!transition.suppressed);

        let mut plain = PreviewPanelLayoutTransition::new(true, 540., false);
        assert!(plain.observe(false, true, 600., false));
        assert!(!plain.suppressed);
    }

    #[test]
    fn frames_without_suppression_do_nothing() {
        let mut transition = PreviewPanelLayoutTransition::new(true, 540., false);
        assert!(!transition.on_frame_painted());
    }
}
