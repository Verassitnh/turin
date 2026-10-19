//! Port of upstream/apps/web/src/components/ui/wizard.tsx
//! Port of upstream/apps/web/src/components/settings/AddProviderInstanceWizardSteps.tsx
//! Port of upstream/apps/web/src/components/settings/AddProviderInstanceWizardSteps.test.tsx
//!
//! `WizardPopup`, `WizardHeader`, `WizardSteps`, `WizardPanel` and
//! `WizardFooter` compose a wizard from the settings dialog frame; the flow
//! logic stays with the caller. Not ported: `AnimatedHeight` (GPUI has no
//! height transition) and the `max-sm:` responsive rules (the window never
//! drops below the `sm` breakpoint).

use crate::theme::color::solid_over;
use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, CursorStyle, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    px, rems,
};

use super::add_provider_instance_logic::{
    ADD_PROVIDER_WIZARD_STEPS, WizardNavigation, WizardPrerequisite, WizardValidation,
    resolve_wizard_navigation,
};
use super::dialog::dialog_frame;
use crate::theme::AppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{self, RADIUS_LG, RADIUS_XL, TEXT_SM};
use crate::ui::{Icon, IconName};

/// `WizardPopup size`: `max-w-xl`, or `max-w-3xl` when wide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WizardSize {
    #[default]
    Default,
    Wide,
}

impl WizardSize {
    fn max_width(self) -> gpui_kit::Rems {
        match self {
            Self::Default => rems(36.),
            Self::Wide => rems(48.),
        }
    }
}

/// `WizardPopup`: the dialog popup holding a header, panel and footer.
pub fn wizard_popup(
    theme: &AppTheme,
    size: WizardSize,
    children: Vec<AnyElement>,
    on_dismiss: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    dialog_frame(
        theme,
        size.max_width(),
        vec![
            div()
                .id("wizard-scroll")
                .flex()
                .flex_col()
                .min_h_0()
                .overflow_y_scroll()
                .children(children)
                .into_any_element(),
        ],
        on_dismiss,
    )
}

/// `WizardHeader`: `DialogHeader` (`flex flex-col gap-2 p-6 pb-3`) holding the
/// title, optional description and any extra content such as the steps.
pub fn wizard_header(
    theme: &AppTheme,
    title: impl Into<SharedString>,
    description: Option<SharedString>,
    children: Vec<AnyElement>,
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .px_6()
        .pt_6()
        .pb(rems(0.75))
        .child(
            div()
                .text_size(rems(1.25))
                .line_height(gpui_kit::relative(1.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.into()),
        )
        .children(description.map(|description| {
            div()
                .text_size(TEXT_SM.size)
                .line_height(TEXT_SM.line_height)
                .text_color(theme.muted_foreground)
                .child(description)
        }))
        .children(children)
        .into_any_element()
}

/// `WizardPanel`: `min-w-0 space-y-4 bg-background/80 px-6 py-5 ring-1
/// ring-black/5 dark:bg-white/2 dark:ring-white/5`.
pub fn wizard_panel(
    theme: &AppTheme,
    min_height: Option<gpui_kit::Rems>,
    child: AnyElement,
) -> AnyElement {
    let dark = theme.is_dark();
    let ring = if dark {
        with_alpha(gpui_kit::white(), 0.05)
    } else {
        with_alpha(gpui_kit::black(), 0.05)
    };
    div()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_4()
        .px_6()
        .py(rems(1.25))
        .border_t_1()
        .border_b_1()
        .border_color(ring)
        .bg(if dark {
            with_alpha(gpui_kit::white(), 0.02)
        } else {
            with_alpha(theme.background, 0.8)
        })
        .when_some(min_height, |this, height| this.min_h(height))
        .child(child)
        .into_any_element()
}

/// `WizardFooter` (a bare `DialogFooter`): `flex justify-end gap-2 px-6 py-4`;
/// `leading` content pushes the buttons to the right edge (`sm:justify-between`).
pub fn wizard_footer(leading: Option<AnyElement>, children: Vec<AnyElement>) -> AnyElement {
    let buttons = div().flex().items_center().gap_2().children(children);
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_6()
        .py_4()
        .when(leading.is_some(), |this| this.justify_between())
        .when(leading.is_none(), |this| this.justify_end())
        .children(leading)
        .child(buttons)
        .into_any_element()
}

/// What one step of `WizardSteps` shows; `aria-current` is `is_current`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WizardStepView {
    pub label: String,
    pub index: usize,
    pub is_current: bool,
    pub is_done: bool,
    /// `aria-label`: `{step}, step {n}[, {summary}]`.
    pub aria_label: String,
    /// The visible label: `{step}[: {summary}]` when summaries are shown.
    pub text: String,
}

/// `WizardSteps`' per-step state.
pub fn wizard_step_views(
    steps: &[&str],
    current_step: usize,
    summaries: &[Option<String>],
    show_summaries: bool,
) -> Vec<WizardStepView> {
    steps
        .iter()
        .enumerate()
        .map(|(index, step)| {
            let summary = summaries
                .get(index)
                .and_then(|summary| summary.as_deref())
                .filter(|summary| !summary.is_empty());
            let done = index < current_step;
            let aria_label = match summary.filter(|_| done) {
                Some(summary) => format!("{step}, step {}, {summary}", index + 1),
                None => format!("{step}, step {}", index + 1),
            };
            let text = match summary.filter(|_| show_summaries && done) {
                Some(summary) => format!("{step}: {summary}"),
                None => (*step).to_owned(),
            };
            WizardStepView {
                label: (*step).to_owned(),
                index,
                is_current: index == current_step,
                is_done: done,
                aria_label,
                text,
            }
        })
        .collect()
}

/// `AddProviderInstanceWizardSteps`' click handler: what a click on a step
/// header resolves to, gating forward jumps through Identity validation.
pub fn wizard_step_click_navigation(
    current_step: usize,
    requested_step: usize,
    step_count: usize,
    instance_id_error: Option<&str>,
    identity_step: Option<usize>,
    prerequisite: Option<(usize, Option<&str>)>,
) -> WizardNavigation {
    resolve_wizard_navigation(
        current_step,
        requested_step as isize,
        step_count,
        &WizardValidation {
            instance_id_error: instance_id_error.map(str::to_owned),
            identity_step,
            prerequisite: prerequisite.map(|(step, error)| WizardPrerequisite {
                step,
                error: error.map(str::to_owned),
            }),
        },
    )
}

/// Inputs of [`add_provider_wizard_steps`] (`AddProviderInstanceWizardStepsProps`).
pub struct AddProviderWizardSteps<'a> {
    pub current_step: usize,
    pub summaries: &'a [Option<String>],
    pub instance_id_error: Option<String>,
    pub steps: &'a [&'a str],
    pub identity_step: Option<usize>,
    pub prerequisite: Option<(usize, Option<String>)>,
    pub disabled: bool,
}

impl<'a> AddProviderWizardSteps<'a> {
    pub fn new(
        current_step: usize,
        summaries: &'a [Option<String>],
        instance_id_error: Option<String>,
    ) -> Self {
        Self {
            current_step,
            summaries,
            instance_id_error,
            steps: &ADD_PROVIDER_WIZARD_STEPS,
            identity_step: None,
            prerequisite: None,
            disabled: false,
        }
    }
}

/// `AddProviderInstanceWizardSteps`: `WizardSteps` whose clicks resolve through
/// [`wizard_step_click_navigation`] and report the result to `on_navigation`.
pub fn add_provider_wizard_steps(
    theme: &AppTheme,
    props: AddProviderWizardSteps<'_>,
    on_navigation: impl Fn(WizardNavigation, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let on_navigation = Rc::new(on_navigation);
    let AddProviderWizardSteps {
        current_step,
        summaries,
        instance_id_error,
        steps,
        identity_step,
        prerequisite,
        disabled,
    } = props;
    let step_count = steps.len();
    wizard_steps(
        theme,
        steps,
        current_step,
        summaries,
        false,
        move |_| disabled,
        move |requested, window, cx| {
            let navigation = wizard_step_click_navigation(
                current_step,
                requested,
                step_count,
                instance_id_error.as_deref(),
                identity_step,
                prerequisite
                    .as_ref()
                    .map(|(step, error)| (*step, error.as_deref())),
            );
            on_navigation(navigation, window, cx);
        },
    )
}

/// `WizardSteps`: the pill row of numbered steps.
pub fn wizard_steps(
    theme: &AppTheme,
    steps: &[&str],
    current_step: usize,
    summaries: &[Option<String>],
    show_summaries: bool,
    is_step_disabled: impl Fn(usize) -> bool,
    on_step_change: impl Fn(usize, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let dark = theme.is_dark();
    let on_step_change = Rc::new(on_step_change);
    let black_5 = with_alpha(gpui_kit::black(), 0.05);
    let white_5 = with_alpha(gpui_kit::white(), 0.05);
    let ring = if dark { white_5 } else { black_5 };
    let mut row = div()
        .id("wizard-steps")
        .flex()
        .gap_1()
        .p_1()
        .rounded(RADIUS_XL)
        .border_1()
        .border_color(ring)
        .bg(if dark {
            with_alpha(gpui_kit::white(), 0.04).into()
        } else {
            theme.background
        });
    for view in wizard_step_views(steps, current_step, summaries, show_summaries) {
        let index = view.index;
        let disabled = is_step_disabled(index);
        let badge = div()
            .flex()
            .size_5()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .rounded_full()
            .text_size(TEXT_SM.size)
            .font_weight(FontWeight::MEDIUM)
            .border_1()
            .when(view.is_done, |this| {
                this.bg(theme.primary)
                    .text_color(theme.primary_foreground)
                    .border_color(theme.primary)
            })
            .when(view.is_current, |this| {
                this.bg(solid_over(theme.primary, 0.1, theme.background))
                    .text_color(theme.primary)
                    .border_color(with_alpha(theme.primary, 0.3))
            })
            .when(!view.is_done && !view.is_current, |this| {
                this.bg(if dark {
                    with_alpha(gpui_kit::white(), 0.05).into()
                } else {
                    theme.card
                })
                .text_color(theme.muted_foreground)
                .border_color(if dark {
                    with_alpha(gpui_kit::white(), 0.10)
                } else {
                    with_alpha(gpui_kit::black(), 0.10)
                })
            })
            .child(if view.is_done {
                Icon::new(IconName::Check)
                    .size(rems(1.))
                    .color(theme.primary_foreground)
                    .into_any_element()
            } else {
                div().child((index + 1).to_string()).into_any_element()
            });
        let on_step_change = on_step_change.clone();
        row = row.child(
            div()
                .id(SharedString::from(format!("wizard-step-{index}")))
                .flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap_2()
                .rounded(RADIUS_LG)
                .px(px(10.))
                .py_2()
                .when(!disabled, |this| this.cursor(CursorStyle::PointingHand))
                .when(view.is_current, |this| {
                    this.bg(theme.card)
                        .border_1()
                        .border_color(ring)
                        .shadow(if dark {
                            Vec::new()
                        } else {
                            tokens::shadow_xs(0.05)
                        })
                })
                .when(!view.is_current && !disabled, |this| {
                    this.hover(|style| style.bg(theme.card))
                })
                .when(!disabled, |this| {
                    this.on_click(move |_, window, cx| on_step_change(index, window, cx))
                })
                .child(badge)
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(TEXT_SM.size)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(if view.is_current {
                            theme.foreground
                        } else {
                            theme.muted_foreground
                        })
                        .child(view.text),
                ),
        );
    }
    row.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    // AddProviderInstanceWizardSteps.test.tsx

    #[test]
    fn gates_the_config_header_click_through_identity_validation() {
        let views = wizard_step_views(&ADD_PROVIDER_WIZARD_STEPS, 0, &[], false);
        assert_eq!(views.len(), ADD_PROVIDER_WIZARD_STEPS.len());
        let navigation = wizard_step_click_navigation(
            0,
            2,
            ADD_PROVIDER_WIZARD_STEPS.len(),
            Some("Instance ID is required."),
            None,
            None,
        );
        assert_eq!(
            navigation,
            WizardNavigation::Blocked {
                step: 1,
                error: "Instance ID is required.".to_owned()
            }
        );
    }

    #[test]
    fn marks_the_wizard_step_separately_from_the_clicked_button_focus() {
        let views = wizard_step_views(
            &ADD_PROVIDER_WIZARD_STEPS,
            1,
            &[Some("Codex".into()), Some("Codex Workspace".into()), None],
            false,
        );
        assert!(!views[0].is_current);
        assert!(views[1].is_current);
        assert!(!views[2].is_current);
    }

    #[test]
    fn preserves_the_backward_header_click() {
        let navigation = wizard_step_click_navigation(
            2,
            0,
            ADD_PROVIDER_WIZARD_STEPS.len(),
            Some("Instance ID is required."),
            None,
            None,
        );
        assert_eq!(navigation, WizardNavigation::Navigate { step: 0 });
    }

    // WizardSteps' aria-label and visible label.

    #[test]
    fn labels_a_finished_step_with_its_summary() {
        let views = wizard_step_views(
            &ADD_PROVIDER_WIZARD_STEPS,
            2,
            &[Some("Codex".into()), Some("Codex Workspace".into()), None],
            true,
        );
        assert_eq!(views[0].aria_label, "Provider, step 1, Codex");
        assert_eq!(views[0].text, "Provider: Codex");
        assert_eq!(views[1].aria_label, "Identity, step 2, Codex Workspace");
        assert_eq!(views[2].aria_label, "Config, step 3");
        assert_eq!(views[2].text, "Config");
    }

    #[test]
    fn keeps_the_summary_out_of_the_visible_label_by_default() {
        let views = wizard_step_views(
            &ADD_PROVIDER_WIZARD_STEPS,
            1,
            &[Some("Codex".into()), None, None],
            false,
        );
        assert_eq!(views[0].text, "Provider");
        assert_eq!(views[0].aria_label, "Provider, step 1, Codex");
    }
}
