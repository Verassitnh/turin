//! Port of upstream/apps/web/src/components/promptLinks/NewThreadLinkView.tsx
#![allow(dead_code)]

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, FontWeight, IntoElement, ParentElement as _, RenderOnce, Styled as _, Window, div,
};
use turin_shared::prompt_links::{NewThreadLink, PromptLinkRepository};

use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{TEXT_SM, TEXT_XL};
use crate::ui::empty::{Empty, empty_description, empty_header, empty_title};
use crate::ui::{Button, ButtonSize, ButtonVariant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NewThreadLinkViewState {
    Opening,
    Opened,
    Failed {
        title: Option<String>,
        message: String,
    },
    MissingRepository {
        repository: PromptLinkRepository,
    },
    PullRequestCheckout,
}

type RetryHandler = Box<dyn Fn(&mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct NewThreadLinkView {
    pub link: NewThreadLink,
    pub state: NewThreadLinkViewState,
    pub on_retry: Option<RetryHandler>,
}

impl RenderOnce for NewThreadLinkView {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        div().child(match self.state {
            NewThreadLinkViewState::Opening => Empty::new()
                .child(
                    empty_header()
                        .child(empty_title("Opening the link...", false))
                        .child(empty_description("Preparing your draft.", cx)),
                )
                .into_any_element(),
            NewThreadLinkViewState::Opened => Empty::new()
                .child(empty_header().child(empty_title("This link is open", false)))
                .into_any_element(),
            NewThreadLinkViewState::Failed { title, message } => div()
                .flex()
                .flex_col()
                .items_center()
                .gap_3()
                .p_6()
                .child(
                    div()
                        .text_size(TEXT_XL.size)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .child(title.unwrap_or_else(|| "Could not open the link".to_owned())),
                )
                .child(
                    div()
                        .text_size(TEXT_SM.size)
                        .text_color(theme.muted_foreground)
                        .child(message),
                )
                .when_some(self.on_retry, |this, handler| {
                    this.child(
                        Button::new("new-thread-link-retry")
                            .size(ButtonSize::Sm)
                            .variant(ButtonVariant::Outline)
                            .label("Try again")
                            .on_click(move |_, window, cx| handler(window, cx)),
                    )
                })
                .into_any_element(),
            NewThreadLinkViewState::MissingRepository { repository } => {
                let name = format!("{}/{}", repository.owner, repository.name);
                Empty::new()
                    .child(
                        empty_header()
                            .child(empty_title(format!("You don't have {name}"), false))
                            .child(empty_description(
                                "Clone it to work on it here, or start without a project.",
                                cx,
                            )),
                    )
                    .into_any_element()
            }
            NewThreadLinkViewState::PullRequestCheckout => {
                let title = match &self.link {
                    NewThreadLink::PullRequest { pull_request, .. } => format!(
                        "Check out {}/{}#{}?",
                        pull_request.owner, pull_request.name, pull_request.number
                    ),
                    _ => "Check out pull request?".to_owned(),
                };
                Empty::new()
                    .child(empty_header().child(empty_title(title, false)))
                    .into_any_element()
            }
        })
    }
}
