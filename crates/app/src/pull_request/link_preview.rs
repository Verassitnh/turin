//! Port of upstream/apps/web/src/components/pullRequest/PullRequestLinkPreview.tsx (the card).
//!
//! The hover card shown over a pull request link: repository and number, the state with its
//! glyph, the title, and the author with how long ago it was opened. Base UI's `PreviewCard`
//! (delayed hover open with `delay=350`, `closeDelay=120`) is the host's job: wrap the link in a
//! hover popover (see `ui::popover`) and render [`link_preview_card`] as its content, built from
//! the detail the source answered with ([`PullRequestPreview`] is enough: it carries every field
//! the card reads).
//!
//! The pure gating (`link_preview_shows_card`, `link_preview_bypasses_confirm`,
//! `link_preview_author_label`) is in `link_pull_request_dialog_logic`.

use gpui_kit::{App, IntoElement, ParentElement as _, SharedString, Styled as _, div, px, rems};
use turin_contracts::pull_request::{PullRequestActor, PullRequestState};

use super::link_pull_request_dialog_logic::link_preview_author_label;
use super::presentation_view::{PullRequestActorAvatar, PullRequestStateGlyph};
use super::pull_request_presentation::resolve_pull_request_state;
use super::relative_time::format_relative_time_label;
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{LEADING_SNUG, TEXT_2XS, TEXT_SM, TEXT_XS};

/// What the card shows.
pub struct LinkPreviewCard<'a> {
    pub repository: &'a str,
    pub number: u64,
    pub state: PullRequestState,
    pub is_draft: bool,
    pub title: &'a str,
    pub author: Option<&'a PullRequestActor>,
    pub created_at: &'a str,
}

/// `PreviewCardPopup` content: `w-80`, `p-3`.
pub fn link_preview_card(card: &LinkPreviewCard<'_>, cx: &App) -> impl IntoElement {
    let theme = cx.app_theme();
    let state = resolve_pull_request_state(card.state, card.is_draft);
    div().w(rems(20.)).max_w(px(640.)).p_3().child(
        div()
            .min_w_0()
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap(rems(0.375))
                    .text_size(TEXT_2XS.size)
                    .text_color(theme.muted_foreground)
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(card.repository.to_owned())),
                    )
                    .child(div().flex_shrink_0().child(format!("#{}", card.number)))
                    .child("·")
                    .child(
                        div()
                            .flex()
                            .flex_shrink_0()
                            .items_center()
                            .gap_1()
                            .child(
                                PullRequestStateGlyph::new(card.state, card.is_draft)
                                    .size(rems(0.75)),
                            )
                            .child(state.label),
                    ),
            )
            .child(
                div()
                    .mt_1()
                    .text_size(TEXT_SM.size)
                    .line_height(rems(TEXT_SM.size.0 * LEADING_SNUG))
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child(SharedString::from(card.title.to_owned())),
            )
            .child(
                div()
                    .mt_2()
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap(rems(0.375))
                    .text_size(TEXT_XS.size)
                    .text_color(theme.muted_foreground)
                    .child(PullRequestActorAvatar::new(card.author))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(link_preview_author_label(card.author))),
                    )
                    .child("·")
                    .child(div().flex_shrink_0().child(format!(
                        "opened {}",
                        format_relative_time_label(card.created_at)
                    ))),
            ),
    )
}
