//! Port of upstream/apps/web/src/components/settings/FoldedSettingsSection.tsx.
//!
//! A grouped settings section that starts closed. The header carries the
//! title, a one line summary of what is set inside, and an optional control
//! such as the section's own switch. A settings search that targets the
//! section opens it.
//!
//! Deviation: the chevron swaps between right and down instead of rotating 90°
//! (GPUI has no element transforms) and the panel opens without the height
//! transition.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, CursorStyle, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div, rems,
};

use super::layout::{SettingsSearchTarget, settings_group};
use super::view::SettingsView;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_MD, TEXT_SM, TEXT_XS};
use crate::ui::{Icon, IconName};

/// `headerPlacement`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FoldedHeaderPlacement {
    #[default]
    Inside,
    Outside,
}

/// A search jump lands inside the fold, so it opens before the scroll runs: once
/// per target (`openedForTarget`).
pub fn should_open_for_search_target(
    search_target_id: Option<&str>,
    id: &str,
    opened_for_target: bool,
) -> bool {
    search_target_id == Some(id) && !opened_for_target
}

pub struct FoldedSection {
    pub id: SharedString,
    pub title: SharedString,
    pub summary: Option<SharedString>,
    pub control: Option<AnyElement>,
    pub placement: FoldedHeaderPlacement,
}

impl FoldedSection {
    pub fn new(id: impl Into<SharedString>, title: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            summary: None,
            control: None,
            placement: FoldedHeaderPlacement::Inside,
        }
    }

    pub fn summary(mut self, summary: Option<impl Into<SharedString>>) -> Self {
        self.summary = summary.map(Into::into);
        self
    }

    pub fn control(mut self, control: impl IntoElement) -> Self {
        self.control = Some(control.into_any_element());
        self
    }

    pub fn placement(mut self, placement: FoldedHeaderPlacement) -> Self {
        self.placement = placement;
        self
    }
}

impl SettingsView {
    /// `<FoldedSettingsSection …>children</FoldedSettingsSection>`.
    pub(super) fn folded_settings_section(
        &mut self,
        section: FoldedSection,
        children: Vec<AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let id = section.id.to_string();
        let search_target = cx
            .try_global::<SettingsSearchTarget>()
            .and_then(|target| target.id.as_ref().map(|id| id.to_string()));
        let opened_for_target = self.panels.folded_opened_for_target.contains(&id);
        if should_open_for_search_target(search_target.as_deref(), &id, opened_for_target) {
            self.panels.folded_opened_for_target.insert(id.clone());
            self.panels.folded_open.insert(id.clone());
        }
        let open = self.panels.folded_open.contains(&id);
        let toggle_id = id.clone();
        let toggle = cx.listener(move |this, _, _, cx| {
            if !this.panels.folded_open.remove(&toggle_id) {
                this.panels.folded_open.insert(toggle_id.clone());
            }
            cx.notify();
        });
        let chevron = |color| {
            Icon::new(if open {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .size(rems(1.))
            .color(color)
        };

        match section.placement {
            FoldedHeaderPlacement::Outside => {
                let header = div()
                    .flex()
                    .min_h_7()
                    .items_start()
                    .justify_between()
                    .gap_4()
                    .px_4()
                    .child(
                        div()
                            .id(SharedString::from(format!("{id}-trigger")))
                            .flex()
                            .min_h_7()
                            .items_center()
                            .gap_2()
                            .rounded(RADIUS_MD)
                            .cursor(CursorStyle::PointingHand)
                            .text_size(TEXT_SM.size)
                            .line_height(TEXT_SM.line_height)
                            .font_weight(FontWeight::NORMAL)
                            .text_color(with_alpha(theme.foreground, 0.7))
                            .on_click(toggle)
                            .child(section.title.clone())
                            .child(chevron(with_alpha(theme.foreground, 0.7).into())),
                    )
                    .children(section.control);
                div()
                    .id(SharedString::from(id))
                    .flex()
                    .flex_col()
                    .gap(rems(0.625))
                    .child(header)
                    .when(open, |this| this.child(settings_group(children, cx)))
                    .into_any_element()
            }
            FoldedHeaderPlacement::Inside => {
                let header =
                    div()
                        .flex()
                        .items_center()
                        .gap_4()
                        .px_4()
                        .child(
                            div()
                                .id(SharedString::from(format!("{id}-trigger")))
                                .flex()
                                .min_h(rems(2.75))
                                .min_w_0()
                                .flex_1()
                                .items_center()
                                .gap_2()
                                .py_2()
                                .rounded(RADIUS_MD)
                                .cursor(CursorStyle::PointingHand)
                                .on_click(toggle)
                                .child(chevron(theme.muted_foreground))
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_size(TEXT_SM.size)
                                        .line_height(TEXT_SM.line_height)
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(section.title.clone()),
                                )
                                .children(section.summary.map(|summary| {
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(TEXT_XS.size)
                                        .text_color(theme.muted_foreground)
                                        .child(summary)
                                })),
                        )
                        .children(section.control.map(|control| {
                            div().flex().flex_shrink_0().items_center().child(control)
                        }));
                let separator = with_alpha(theme.border, 0.5);
                let panel = div()
                    .flex()
                    .flex_col()
                    .border_t_1()
                    .border_color(separator)
                    .children(children.into_iter().enumerate().map(move |(index, child)| {
                        div()
                            .when(index > 0, |this| this.border_t_1().border_color(separator))
                            .child(child)
                    }));
                // `SettingsGroup divided={false}`.
                let group = settings_group(Vec::new(), cx);
                div()
                    .id(SharedString::from(id))
                    .child(group.child(header).when(open, |this| this.child(panel)))
                    .into_any_element()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_once_for_a_search_target() {
        assert!(should_open_for_search_target(
            Some("runtime"),
            "runtime",
            false
        ));
        assert!(!should_open_for_search_target(
            Some("runtime"),
            "runtime",
            true
        ));
        assert!(!should_open_for_search_target(
            Some("other"),
            "runtime",
            false
        ));
        assert!(!should_open_for_search_target(None, "runtime", false));
    }
}
