//! Port of upstream/apps/web/src/components/ProjectFavicon.tsx
//!
//! A project's icon: its configured monogram/emoji/lucide icon, else its favicon image
//! (`projectFaviconUrlAtom`: found in the workspace, cached on disk, see
//! `project_favicon_images`), else the generated monogram while there is none.

use gpui_kit::{
    App, IntoElement, ObjectFit, ParentElement as _, Rems, RenderOnce, Styled as _,
    StyledImage as _, Window, div, img, rems,
};

use super::project_monogram::ProjectMonogram;
use crate::project_favicon_images::ProjectFaviconImages;
use crate::project_icon_colors::project_icon_color;
use crate::project_identity::derive_project_identity;
use crate::state::models::{EnvironmentProject, ProjectIcon};
use crate::theme::ActiveAppTheme;
use crate::ui::{Icon, IconName};

#[derive(IntoElement)]
pub struct ProjectFavicon {
    /// Kept whole so the icon image can be asked for by the project's own record.
    project: EnvironmentProject,
    title: String,
    icon: Option<ProjectIcon>,
    size: Rems,
    grayscale: bool,
}

impl ProjectFavicon {
    pub fn new(project: &EnvironmentProject) -> Self {
        Self {
            project: project.clone(),
            title: project.title.clone(),
            icon: project.project_icon.clone(),
            size: rems(1.),
            grayscale: false,
        }
    }

    /// `grayscale` (settled history at rest): GPUI has no CSS filters, so
    /// the tint is desaturated instead.
    pub fn grayscale(mut self, grayscale: bool) -> Self {
        self.grayscale = grayscale;
        self
    }

    /// `className="size-*"`.
    pub fn size(mut self, size: Rems) -> Self {
        self.size = size;
        self
    }
}

impl RenderOnce for ProjectFavicon {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        // Only a project with no icon of its own shows a favicon image.
        let favicon = if self.icon.is_none() {
            ProjectFaviconImages::image(cx, &self.project)
        } else {
            None
        };
        let theme = cx.app_theme();
        let dark = theme.is_dark();
        let gray = self.grayscale;
        let project_icon_color = |color, dark| {
            let mut tint = project_icon_color(color, dark);
            if gray {
                tint.s = 0.;
            }
            tint
        };
        match self.icon {
            Some(ProjectIcon::Monogram { text, color }) => {
                ProjectMonogram::new(text, project_icon_color(color, dark))
                    .size(self.size)
                    .into_any_element()
            }
            Some(ProjectIcon::Emoji(emoji)) => div()
                .flex()
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .size(self.size)
                .text_size(self.size * 0.8)
                .child(emoji)
                .into_any_element(),
            // Lucide icons are named dynamically upstream; the catalog here
            // is static, so the folder glyph stands in, tinted.
            Some(ProjectIcon::Lucide { color, .. }) => Icon::new(IconName::FolderCode)
                .size(self.size)
                .color(project_icon_color(color, dark))
                .into_any_element(),
            // `rounded-[25%] object-contain`.
            None if favicon.is_some() => div()
                .flex_shrink_0()
                .size(self.size)
                .overflow_hidden()
                .rounded(self.size * 0.25)
                .child(
                    img(favicon.expect("checked above"))
                        .size_full()
                        .object_fit(ObjectFit::Contain),
                )
                .into_any_element(),
            None if !self.title.trim().is_empty() => {
                let identity = derive_project_identity(&self.title);
                ProjectMonogram::new(identity.monogram, project_icon_color(identity.color, dark))
                    .size(self.size)
                    .into_any_element()
            }
            None => Icon::new(IconName::FolderCode)
                .size(self.size)
                .color(theme.icon_muted)
                .into_any_element(),
        }
    }
}
