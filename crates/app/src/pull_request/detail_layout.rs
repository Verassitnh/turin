//! Port of upstream/apps/web/src/components/pullRequest/PullRequestDetailLayout.tsx: the header
//! body (title, author line, branch line), the title row, the tab bar and the checks status line
//! that the panel and its loading ghost both assemble.
//!
//! The `@container/pr-header` queries (`@max-[30rem]/pr-header:hidden` and friends) are decided by
//! the caller from the width of the panel; here they are plain flags.

use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, Div, Hsla, IntoElement, ParentElement as _, SharedString,
    Styled as _, Window, div, px, rems,
};

use super::detail_context::DetailTab;
use super::presentation_view::pull_request_meta_line;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_LG, TEXT_XS};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::{Icon, IconName, Toggle, ToggleSize, ToggleVariant};

/// `TABS`: the tab strip's values and labels.
pub const TABS: [(DetailTab, &str); 4] = [
    (DetailTab::Summary, "Summary"),
    (DetailTab::Guide, "Guide"),
    (DetailTab::Timeline, "Timeline"),
    (DetailTab::Code, "Code"),
];

/// The width of the panel below which the labelled header buttons shrink to their glyph
/// (`@max-[30rem]/pr-header:hidden`).
pub const HEADER_LABELS_MIN_WIDTH: f32 = 480.;
/// `@max-[35rem]/pr-header:hidden`, the checkout button's label.
pub const HEADER_CHECKOUT_LABEL_MIN_WIDTH: f32 = 560.;

/// `color-mix(…/alpha)` shorthand as an `Hsla`.
pub fn alpha(color: Hsla, alpha: f32) -> Hsla {
    with_alpha(color, alpha).into()
}

/// `PullRequestDetailTitleRow`: `flex min-h-7 min-w-0 items-center sm:min-h-6`.
pub fn title_row() -> Div {
    div().flex().min_h(rems(1.5)).min_w_0().items_center()
}

/// The slots of `PullRequestDetailHeaderBody`.
pub struct HeaderBody {
    pub title: AnyElement,
    pub author: AnyElement,
    pub updated: AnyElement,
    pub checkout: Option<AnyElement>,
    pub base: AnyElement,
    pub head: AnyElement,
    pub files: AnyElement,
    pub diff_stat: AnyElement,
}

/// `PullRequestDetailHeaderBody`: `mt-1 min-w-0 px-4 pb-4` around the title, the author line and
/// the branch line.
pub fn header_body(body: HeaderBody, theme: &AppTheme) -> Div {
    let muted = theme.muted_foreground;
    div()
        .flex()
        .flex_col()
        .min_w_0()
        .mt_1()
        .px_4()
        .pb_4()
        .child(body.title)
        .child(
            div()
                .flex()
                .mt_2()
                .min_h(rems(1.25))
                .min_w_0()
                .items_center()
                .gap_2()
                .text_size(TEXT_XS.size)
                .text_color(muted)
                .child(pull_request_meta_line(
                    [Some(body.author), Some(body.updated)],
                    theme,
                ))
                .children(body.checkout),
        )
        .child(
            div()
                .flex()
                .mt_4()
                .min_h(rems(1.25))
                .min_w_0()
                .items_center()
                .gap_2()
                .text_size(TEXT_XS.size)
                .text_color(muted)
                .child(
                    div()
                        .flex()
                        .flex_1()
                        .min_w_0()
                        .items_center()
                        .gap(rems(0.375))
                        .font_family(theme.fonts.mono.clone())
                        .text_size(TEXT_XS.size)
                        .text_color(alpha(muted, 0.7))
                        .child(body.base)
                        .child(
                            Icon::new(IconName::ArrowLeft)
                                .size(rems(0.875))
                                .color(alpha(muted, 0.6)),
                        )
                        .child(body.head),
                )
                .child(
                    div()
                        .flex()
                        .flex_shrink_0()
                        .ml_auto()
                        .items_center()
                        .justify_end()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .min_w(rems(4.))
                                .items_center()
                                .justify_end()
                                .gap(rems(0.375))
                                .child(Icon::new(IconName::FileDiff).size(rems(0.875)))
                                .child(body.files),
                        )
                        .child(body.diff_stat),
                ),
        )
}

pub type SelectTab = Rc<dyn Fn(DetailTab, &ClickEvent, &mut Window, &mut App)>;

/// `PullRequestDetailTabBar`: `flex flex-wrap items-center gap-2 border-t border-border/60
/// px-4 py-2` around the segmented tabs and whatever the active tab puts beside them.
pub fn tab_bar(
    tabs: &[DetailTab],
    value: DetailTab,
    on_select: Option<SelectTab>,
    trailing: Option<AnyElement>,
    theme: &AppTheme,
) -> Div {
    let toggles = tabs.iter().map(|tab| {
        let label = TABS
            .iter()
            .find(|(candidate, _)| candidate == tab)
            .map_or("", |(_, label)| *label);
        let tab = *tab;
        let toggle = Toggle::new(SharedString::from(format!("pr-tab-{label}")))
            .variant(ToggleVariant::Segmented)
            .size(ToggleSize::Segmented)
            .pressed(tab == value)
            .label(label);
        match on_select.clone() {
            Some(on_select) => toggle
                .on_click(move |event, window, cx| on_select(tab, event, window, cx))
                .into_any_element(),
            None => toggle.into_any_element(),
        }
    });
    div()
        .flex()
        .flex_wrap()
        .min_w_0()
        .items_center()
        .gap_2()
        .px_4()
        .py_2()
        .border_t_1()
        .border_color(alpha(theme.border, 0.6))
        // `ToggleGroup`: `flex w-fit gap-0.5 rounded-lg bg-input/40 p-0.5`.
        .child(
            div()
                .flex()
                .flex_shrink_0()
                .gap(px(2.))
                .p(px(2.))
                .rounded(RADIUS_LG)
                .bg(alpha(theme.input, 0.4))
                .children(toggles),
        )
        .children(trailing)
}

/// `PullRequestChecksStatusLine`: `ml-auto flex h-4 min-w-0 flex-1 items-center justify-end
/// gap-x-1.5 overflow-hidden text-xs`.
pub fn checks_status_line(icon: AnyElement, label: impl Into<SharedString>, color: Hsla) -> Div {
    div()
        .flex()
        .ml_auto()
        .h(rems(1.))
        .min_w_0()
        .flex_1()
        .items_center()
        .justify_end()
        .gap(rems(0.375))
        .overflow_hidden()
        .text_size(TEXT_XS.size)
        .text_color(color)
        .child(icon)
        .child(div().whitespace_nowrap().child(label.into()))
}

/// The tabs the strip offers: Code goes where the host has no patch to show, Guide where the
/// reader has not opted into the beta (`visibleTabs`).
pub fn visible_tabs(has_diff: bool, guides_enabled: bool) -> Vec<DetailTab> {
    TABS.iter()
        .map(|(tab, _)| *tab)
        .filter(|tab| *tab != DetailTab::Code || has_diff)
        .filter(|tab| *tab != DetailTab::Guide || guides_enabled)
        .collect()
}

/// `x.toLocaleString()` for counts (en-US grouping).
pub fn format_count(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// `1 file` / `3 files`.
pub fn files_label(changed_files: u64) -> String {
    format!(
        "{} {}",
        format_count(changed_files),
        if changed_files == 1 { "file" } else { "files" }
    )
}

#[allow(dead_code)]
fn _unused(_: &App) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_counts_like_to_locale_string() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1000), "1,000");
        assert_eq!(format_count(1234567), "1,234,567");
    }

    #[test]
    fn labels_files_in_the_singular() {
        assert_eq!(files_label(1), "1 file");
        assert_eq!(files_label(12), "12 files");
    }

    #[test]
    fn drops_code_without_a_patch_and_guide_without_the_beta() {
        assert_eq!(
            visible_tabs(true, true),
            vec![
                DetailTab::Summary,
                DetailTab::Guide,
                DetailTab::Timeline,
                DetailTab::Code
            ]
        );
        assert_eq!(
            visible_tabs(false, false),
            vec![DetailTab::Summary, DetailTab::Timeline]
        );
    }
}
