//! `PublishRepositoryDialog` of GitActionsControl.tsx: the three-step wizard
//! (Provider, Repository, Summary) that creates a repository on the host and
//! pushes the checkout to it. Turin ships GitHub only, so the provider step
//! is one card (or "Setup Required" when `gh` is not ready).

use gpui_kit::AppContext as _;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Sizable as _, Size};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, CursorStyle, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _, Subscription,
    WeakEntity, Window, div, px, rems,
};
use turin_contracts::source_control::{
    SourceControlCloneProtocol, SourceControlPublishRepositoryResult, SourceControlPublishStatus,
    SourceControlRepositoryVisibility,
};

use super::GitActionsControl;
use super::backend::{GitActionTarget, PublishProviderState, PublishRepositoryInput};
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_LG, RADIUS_MD, TEXT_SM, TEXT_XS};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::icon::{Icon, IconName};
use crate::ui::{Button, ButtonSize, ButtonVariant};

const STEPS: [&str; 3] = ["Provider", "Repository", "Summary"];

/// `canSubmitPublishRepository`'s repository check: `owner/name`.
pub fn is_publishable_repository(value: &str) -> bool {
    let mut parts = value.trim().split('/');
    let owner = parts.next().unwrap_or("").trim();
    let name = parts.collect::<Vec<_>>().join("/");
    !owner.is_empty() && !name.trim().is_empty()
}

pub struct PublishForm {
    control: WeakEntity<GitActionsControl>,
    target: GitActionTarget,
    provider: PublishProviderState,
    step: usize,
    repository: Entity<InputState>,
    remote_name: Entity<InputState>,
    visibility: SourceControlRepositoryVisibility,
    protocol: SourceControlCloneProtocol,
    advanced_open: bool,
    pending: bool,
    error: Option<String>,
    result: Option<SourceControlPublishRepositoryResult>,
    _subscriptions: Vec<Subscription>,
}

impl PublishForm {
    fn new(
        control: WeakEntity<GitActionsControl>,
        target: GitActionTarget,
        provider: PublishProviderState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let prefill = provider
            .account
            .as_ref()
            .map(|account| format!("{account}/"))
            .unwrap_or_default();
        let repository = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("owner/repo")
                .default_value(prefill)
        });
        let remote_name = cx.new(|cx| InputState::new(window, cx).default_value("origin"));
        let subscriptions = vec![
            cx.subscribe_in(&repository, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => this.submit(window, cx),
                InputEvent::Change => cx.notify(),
                _ => {}
            }),
            cx.subscribe(&remote_name, |_, _, _: &InputEvent, cx| cx.notify()),
        ];
        Self {
            control,
            target,
            provider,
            step: 0,
            repository,
            remote_name,
            visibility: SourceControlRepositoryVisibility::Private,
            protocol: SourceControlCloneProtocol::Ssh,
            advanced_open: false,
            pending: false,
            error: None,
            result: None,
            _subscriptions: subscriptions,
        }
    }

    fn repository_value(&self, cx: &gpui_kit::App) -> String {
        self.repository.read(cx).value().to_string()
    }

    fn can_submit(&self, cx: &gpui_kit::App) -> bool {
        self.provider.ready && !self.pending && is_publishable_repository(&self.repository_value(cx))
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_submit(cx) {
            return;
        }
        self.error = None;
        self.pending = true;
        let remote = self.remote_name.read(cx).value().trim().to_owned();
        let input = PublishRepositoryInput {
            repository: self.repository_value(cx).trim().to_owned(),
            visibility: self.visibility,
            remote_name: if remote.is_empty() { "origin".to_owned() } else { remote },
            protocol: self.protocol,
        };
        let backend = self
            .control
            .upgrade()
            .map(|control| control.read(cx).backend.clone());
        let Some(backend) = backend else {
            return;
        };
        let task = backend.publish_repository(&self.target, input, cx);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let outcome = task.await;
            this.update_in(cx, |this, _, cx| {
                this.pending = false;
                match outcome {
                    Ok(result) => {
                        this.result = Some(result);
                        this.step = 2;
                    }
                    Err(message) => this.error = Some(message),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

fn card_border(selected: bool, theme: &AppTheme) -> (gpui_kit::Hsla, gpui_kit::Hsla) {
    // `border-primary bg-background ring-primary/35` selected; `border-border` otherwise.
    if selected {
        (theme.primary, theme.background)
    } else {
        (theme.border, theme.background)
    }
}

fn label(text: &'static str) -> gpui_kit::Div {
    div()
        .text_size(TEXT_XS.size)
        .font_weight(FontWeight::MEDIUM)
        .child(text)
}

impl Render for PublishForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let weak = cx.entity().downgrade();
        let step = self.step;

        // WizardSteps: numbered, the current one emphasised, summaries under.
        let steps = div().flex().items_center().gap_3().children(STEPS.iter().enumerate().map(
            |(index, name)| {
                let current = index == step;
                let done = index < step;
                let summary = match index {
                    0 if step > 0 => Some(self.provider.label.clone()),
                    1 => self
                        .result
                        .as_ref()
                        .map(|result| result.repository.name_with_owner.as_str().to_owned()),
                    _ => None,
                };
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .size(rems(1.25))
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .text_size(TEXT_XS.size)
                            .bg(if current || done { theme.primary } else { theme.muted })
                            .text_color(if current || done {
                                theme.primary_foreground
                            } else {
                                theme.muted_foreground
                            })
                            .child(if done {
                                Icon::new(IconName::Check)
                                    .size(rems(0.75))
                                    .color(theme.primary_foreground)
                                    .into_any_element()
                            } else {
                                div().child((index + 1).to_string()).into_any_element()
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(TEXT_SM.size)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(if current {
                                        theme.foreground
                                    } else {
                                        theme.muted_foreground
                                    })
                                    .child(*name),
                            )
                            .children(summary.map(|summary| {
                                div()
                                    .text_size(TEXT_XS.size)
                                    .text_color(theme.muted_foreground)
                                    .child(summary)
                            })),
                    )
            },
        ));

        let body = match step {
            0 => {
                let ready = self.provider.ready;
                let (border, background) = card_border(ready, &theme);
                let hint = self.provider.hint.clone();
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(label("Provider"))
                    .child(
                        div()
                            .w(rems(14.))
                            .flex()
                            .items_center()
                            .gap_3()
                            .rounded(RADIUS_LG)
                            .border_1()
                            .border_color(border)
                            .bg(background)
                            .px_3()
                            .py_3()
                            .when(ready, |this| this.cursor(CursorStyle::PointingHand))
                            .when(!ready, |this| this.opacity(0.64))
                            .child(Icon::new(IconName::Github).size(rems(1.25)).color(theme.foreground))
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .truncate()
                                    .text_size(TEXT_SM.size)
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(self.provider.label.clone()),
                            )
                            .when(!ready, |this| {
                                this.child(
                                    Button::new("publish-setup-required")
                                        .variant(ButtonVariant::WarningOutline)
                                        .size(ButtonSize::Micro)
                                        .label("Setup Required"),
                                )
                            }),
                    )
                    .children(hint.filter(|_| !ready).map(|hint| {
                        div()
                            .max_w(rems(24.))
                            .text_size(TEXT_XS.size)
                            .text_color(theme.muted_foreground)
                            .child(hint)
                    }))
                    .into_any_element()
            }
            1 => {
                let host = self.provider.host.clone();
                let visibility = self.visibility;
                let protocol = self.protocol;
                let advanced_open = self.advanced_open;
                let weak_visibility = weak.clone();
                let weak_advanced = weak.clone();
                let weak_protocol = weak.clone();
                let option = |id: &'static str,
                              value: SourceControlRepositoryVisibility,
                              title: &'static str,
                              description: &'static str,
                              icon: IconName,
                              weak: WeakEntity<PublishForm>,
                              theme: &AppTheme| {
                    let selected = visibility == value;
                    let (border, background) = card_border(selected, theme);
                    div()
                        .id(id)
                        .flex()
                        .flex_1()
                        .items_center()
                        .gap_3()
                        .rounded(RADIUS_LG)
                        .border_1()
                        .border_color(border)
                        .bg(background)
                        .px_3()
                        .py(rems(0.625))
                        .cursor(CursorStyle::PointingHand)
                        .on_click(move |_, _, cx| {
                            weak.update(cx, |this, cx| {
                                this.visibility = value;
                                cx.notify();
                            })
                            .ok();
                        })
                        .child(Icon::new(icon).color(theme.muted_foreground))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_size(TEXT_SM.size)
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .text_size(TEXT_XS.size)
                                        .text_color(theme.muted_foreground)
                                        .child(description),
                                ),
                        )
                };
                div()
                    .flex()
                    .flex_col()
                    .gap_5()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(label("Repository"))
                            .child(
                                div()
                                    .flex()
                                    .items_stretch()
                                    .overflow_hidden()
                                    .rounded(RADIUS_MD)
                                    .border_1()
                                    .border_color(theme.input)
                                    .child(
                                        div()
                                            .flex()
                                            .flex_shrink_0()
                                            .items_center()
                                            .gap(rems(0.375))
                                            .border_r_1()
                                            .border_color(theme.input)
                                            .bg(with_alpha(theme.muted, 0.5))
                                            .px(rems(0.625))
                                            .font_family(theme.fonts.mono.clone())
                                            .text_size(TEXT_XS.size)
                                            .text_color(theme.muted_foreground)
                                            .child(Icon::new(IconName::Github).size(rems(0.875)))
                                            .child(format!("{host}/")),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .px_3()
                                            .font_family(theme.fonts.mono.clone())
                                            .text_size(TEXT_SM.size)
                                            .child(Input::new(&self.repository).appearance(false).bordered(false).p_0()),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(label("Visibility"))
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .child(option(
                                        "publish-private",
                                        SourceControlRepositoryVisibility::Private,
                                        "Private",
                                        "Only invited people",
                                        IconName::Lock,
                                        weak_visibility.clone(),
                                        &theme,
                                    ))
                                    .child(option(
                                        "publish-public",
                                        SourceControlRepositoryVisibility::Public,
                                        "Public",
                                        "Anyone on the web",
                                        IconName::Globe,
                                        weak_visibility,
                                        &theme,
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                div()
                                    .id("publish-advanced")
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .cursor(CursorStyle::PointingHand)
                                    .text_size(TEXT_XS.size)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.muted_foreground)
                                    .on_click(move |_, _, cx| {
                                        weak_advanced
                                            .update(cx, |this, cx| {
                                                this.advanced_open = !this.advanced_open;
                                                cx.notify();
                                            })
                                            .ok();
                                    })
                                    .child(
                                        Icon::new(if advanced_open {
                                            IconName::ChevronDown
                                        } else {
                                            IconName::ChevronRight
                                        })
                                        .size(rems(0.875)),
                                    )
                                    .child("Advanced"),
                            )
                            .when(advanced_open, |this| {
                                this.child(
                                    div()
                                        .flex()
                                        .gap_3()
                                        .child(
                                            div()
                                                .flex()
                                                .flex_col()
                                                .flex_1()
                                                .gap(rems(0.375))
                                                .child(label("Remote"))
                                                .child(Input::new(&self.remote_name)),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .flex_col()
                                                .gap(rems(0.375))
                                                .child(label("Protocol"))
                                                .child(div().flex().gap_1().children(
                                                    [
                                                        ("SSH", SourceControlCloneProtocol::Ssh),
                                                        ("HTTPS", SourceControlCloneProtocol::Https),
                                                    ]
                                                    .into_iter()
                                                    .map(|(name, value)| {
                                                        let weak = weak_protocol.clone();
                                                        Button::new(name)
                                                            .variant(if protocol == value {
                                                                ButtonVariant::Secondary
                                                            } else {
                                                                ButtonVariant::Outline
                                                            })
                                                            .size(ButtonSize::Sm)
                                                            .label(name)
                                                            .on_click(move |_, _, cx| {
                                                                weak.update(cx, |this, cx| {
                                                                    this.protocol = value;
                                                                    cx.notify();
                                                                })
                                                                .ok();
                                                            })
                                                    }),
                                                )),
                                        ),
                                )
                            }),
                    )
                    .when(self.pending, |this| {
                        this.child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .rounded(RADIUS_MD)
                                .border_1()
                                .border_color(theme.input)
                                .bg(with_alpha(theme.muted, 0.4))
                                .px_3()
                                .py_2()
                                .text_size(TEXT_XS.size)
                                .text_color(theme.muted_foreground)
                                .child(Spinner::new().with_size(Size::XSmall))
                                .child(format!("Publishing repository to {}...", self.provider.label)),
                        )
                    })
                    .children(self.error.clone().filter(|_| !self.pending).map(|error| {
                        div()
                            .rounded(RADIUS_MD)
                            .border_1()
                            .border_color(with_alpha(theme.destructive, 0.4))
                            .bg(with_alpha(theme.destructive, 0.1))
                            .px_3()
                            .py_2()
                            .text_size(TEXT_XS.size)
                            .text_color(theme.destructive_foreground)
                            .child(div().font_weight(FontWeight::MEDIUM).child("Publish failed"))
                            .child(div().mt(px(2.)).child(error))
                    }))
                    .into_any_element()
            }
            _ => match &self.result {
                Some(result) => {
                    let pushed = result.status == SourceControlPublishStatus::Pushed;
                    let provider = self.provider.label.clone();
                    let url = result.repository.url.as_str().to_owned();
                    let weak_open = self.control.clone();
                    div()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .items_center()
                                .gap_2()
                                .py_1()
                                .child(
                                    div()
                                        .flex()
                                        .size(rems(2.))
                                        .items_center()
                                        .justify_center()
                                        .rounded_full()
                                        .bg(with_alpha(theme.success, 0.15))
                                        .child(Icon::new(IconName::Check).color(theme.success)),
                                )
                                .child(
                                    div()
                                        .text_size(TEXT_SM.size)
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(if pushed {
                                            "Repository published"
                                        } else {
                                            "Repository created"
                                        }),
                                )
                                .child(
                                    div()
                                        .max_w(rems(20.))
                                        .text_size(TEXT_XS.size)
                                        .text_color(theme.muted_foreground)
                                        .child(if pushed {
                                            format!("{} is now live on {provider}.", result.branch)
                                        } else {
                                            format!(
                                                "Remote \"{}\" is set up. Make a commit and push it to share your code.",
                                                result.remote_name
                                            )
                                        }),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .rounded(RADIUS_LG)
                                .border_1()
                                .border_color(theme.input)
                                .bg(with_alpha(theme.muted, 0.4))
                                .px_3()
                                .py_2()
                                .child(Icon::new(IconName::Github).size(rems(0.875)).color(theme.muted_foreground))
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .truncate()
                                        .font_family(theme.fonts.mono.clone())
                                        .text_size(TEXT_XS.size)
                                        .child(result.repository.name_with_owner.as_str().to_owned()),
                                ),
                        )
                        .child(
                            Button::new("publish-open")
                                .variant(ButtonVariant::Outline)
                                .size(ButtonSize::Sm)
                                .label(format!("Open on {provider}"))
                                .on_click(move |_, _, cx| {
                                    let url = url.clone();
                                    weak_open
                                        .update(cx, |control, cx| control.backend.open_link(url, cx))
                                        .ok();
                                }),
                        )
                        .into_any_element()
                }
                None => div()
                    .rounded(RADIUS_MD)
                    .border_1()
                    .border_color(theme.input)
                    .px_3()
                    .py_2()
                    .text_size(TEXT_XS.size)
                    .text_color(theme.muted_foreground)
                    .child("Publish result unavailable.")
                    .into_any_element(),
            },
        };

        let can_submit = self.can_submit(cx);
        let pending = self.pending;
        let ready = self.provider.ready;
        let weak_back = weak.clone();
        let weak_next = weak.clone();
        let weak_publish = weak.clone();
        let footer = if step == 2 {
            div().flex().justify_end().gap_2().child(
                Button::new("publish-done")
                    .variant(ButtonVariant::Default)
                    .size(ButtonSize::Sm)
                    .label("Done")
                    .on_click(|_, window, cx| window.close_dialog(cx)),
            )
        } else {
            div()
                .flex()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("publish-back")
                        .variant(ButtonVariant::Outline)
                        .size(ButtonSize::Sm)
                        .label(if step == 0 { "Cancel" } else { "Back" })
                        .disabled(pending)
                        .on_click(move |_, window, cx| {
                            if step == 0 {
                                window.close_dialog(cx);
                            } else {
                                weak_back
                                    .update(cx, |this, cx| {
                                        this.step = this.step.saturating_sub(1);
                                        cx.notify();
                                    })
                                    .ok();
                            }
                        }),
                )
                .child(if step < 1 {
                    Button::new("publish-next")
                        .variant(ButtonVariant::Default)
                        .size(ButtonSize::Sm)
                        .label("Next")
                        .disabled(!ready)
                        .on_click(move |_, _, cx| {
                            weak_next
                                .update(cx, |this, cx| {
                                    this.step = 1;
                                    cx.notify();
                                })
                                .ok();
                        })
                } else {
                    Button::new("publish-submit")
                        .variant(ButtonVariant::Default)
                        .size(ButtonSize::Sm)
                        .label(if pending { "Publishing..." } else { "Publish" })
                        .disabled(!can_submit)
                        .on_click(move |_, window, cx| {
                            weak_publish
                                .update(cx, |this, cx| this.submit(window, cx))
                                .ok();
                        })
                })
        };

        div()
            .flex()
            .flex_col()
            .gap_4()
            .w(px(520.))
            .child(
                div()
                    .text_size(TEXT_SM.size)
                    .text_color(theme.muted_foreground)
                    .child("Pick where to host it, then point us at a repo to push to."),
            )
            .child(steps)
            .child(body)
            .child(footer)
    }
}

/// Opens the wizard for the control's checkout.
pub fn open(control: WeakEntity<GitActionsControl>, window: &mut Window, cx: &mut gpui_kit::App) {
    let Some(entity) = control.upgrade() else {
        return;
    };
    let Some(target) = entity.read(cx).current_target() else {
        return;
    };
    let provider = entity.read(cx).backend.publish_provider(cx);
    let form = cx.new(|cx| PublishForm::new(control, target, provider, window, cx));
    window.open_dialog(cx, move |dialog, _, _| {
        dialog.title("Publish repository").child(form.clone())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repository_needs_an_owner_and_a_name() {
        assert!(is_publishable_repository("acme/turin"));
        assert!(is_publishable_repository("  acme/team/turin "));
        assert!(!is_publishable_repository("acme/"));
        assert!(!is_publishable_repository("acme"));
        assert!(!is_publishable_repository("/turin"));
        assert!(!is_publishable_repository(""));
    }
}
