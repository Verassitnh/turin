//! Port of upstream/apps/web/src/components/ui/refresh-icon.tsx
//!
//! Sizes: `xs` 12px, `sm` 14px, `md` 16px, `lg` 20px. No default size, matching
//! upstream (a parent button's svg rule sizes an unsized glyph; here the
//! caller passes [`RefreshIconSize`] or the icon stays `size-4`).
//!
//! Not ported: `animate-spin`. GPUI elements have no transform, the same limit
//! as `Button`'s dropped `active:scale`. `refreshing` is still tracked so a
//! caller can read it; the glyph does not rotate.

use gpui_kit::{App, IntoElement, Rems, RenderOnce, Window, rems};

use super::icon::{Icon, IconName};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RefreshIconSize {
    Xs,
    Sm,
    #[default]
    Md,
    Lg,
}

fn size_rems(size: RefreshIconSize) -> Rems {
    match size {
        RefreshIconSize::Xs => rems(0.75),
        RefreshIconSize::Sm => rems(0.875),
        RefreshIconSize::Md => rems(1.),
        RefreshIconSize::Lg => rems(1.25),
    }
}

#[derive(IntoElement)]
pub struct RefreshIcon {
    size: Option<RefreshIconSize>,
    refreshing: bool,
}

impl RefreshIcon {
    pub fn new() -> Self {
        Self {
            size: None,
            refreshing: false,
        }
    }

    pub fn size(mut self, size: RefreshIconSize) -> Self {
        self.size = Some(size);
        self
    }

    pub fn refreshing(mut self, refreshing: bool) -> Self {
        self.refreshing = refreshing;
        self
    }

    pub fn is_refreshing(&self) -> bool {
        self.refreshing
    }
}

impl Default for RefreshIcon {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderOnce for RefreshIcon {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let _ = self.refreshing;
        let size = self.size.map(size_rems).unwrap_or(rems(1.));
        Icon::new(IconName::RefreshCw).size(size)
    }
}
