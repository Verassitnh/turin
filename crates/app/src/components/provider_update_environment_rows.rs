//! Port of upstream/apps/web/src/components/ProviderUpdateEnvironmentRows.tsx
#![allow(dead_code)]

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, FontWeight, IntoElement, ParentElement as _, RenderOnce, Styled as _, Window, div, rems,
};
use turin_contracts::EnvironmentId;

use super::provider_update_launch_notification_logic::{
    LocalEnvironmentUpdateGroup, LocalProviderUpdateOutcome, ProviderUpdateRowStatus,
    ProviderUpdateRowStatusKind,
};
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::TEXT_XS;
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName};
use turin_provider_core::server_provider::ServerProvider;

pub const PENDING_EXPIRY_MS: u64 = 6 * 60_000;

#[derive(Clone, Debug)]
pub enum ProviderUpdateCommandResult {
    Success { providers: Vec<ServerProvider> },
    Failed { interrupted: bool, message: String },
}

pub fn to_provider_update_outcome(
    environment_id: EnvironmentId,
    is_primary: bool,
    target: &ServerProvider,
    result: &ProviderUpdateCommandResult,
) -> Result<LocalProviderUpdateOutcome, String> {
    match result {
        ProviderUpdateCommandResult::Failed {
            interrupted: true, ..
        } => Ok(LocalProviderUpdateOutcome {
            environment_id,
            is_primary,
            driver: target.driver.clone(),
            instance_id: target.instance_id.clone(),
            provider: None,
        }),
        ProviderUpdateCommandResult::Failed { message, .. } => Err(message.clone()),
        ProviderUpdateCommandResult::Success { providers } => Ok(LocalProviderUpdateOutcome {
            environment_id,
            is_primary,
            driver: target.driver.clone(),
            instance_id: target.instance_id.clone(),
            provider: providers
                .iter()
                .find(|provider| provider.instance_id == target.instance_id)
                .cloned(),
        }),
    }
}

fn row_tone_color(
    kind: ProviderUpdateRowStatusKind,
    theme: &crate::theme::AppTheme,
) -> gpui_kit::Hsla {
    match kind {
        ProviderUpdateRowStatusKind::Failed => theme.destructive,
        ProviderUpdateRowStatusKind::Unchanged => theme.warning,
        ProviderUpdateRowStatusKind::Success => theme.success,
        _ => theme.muted_foreground,
    }
}

type UpdateHandler = Box<dyn Fn(&mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct EnvironmentUpdateRow {
    pub group: LocalEnvironmentUpdateGroup,
    pub status: ProviderUpdateRowStatus,
    pub can_manage_providers: bool,
    pub on_update: UpdateHandler,
}

impl RenderOnce for EnvironmentUpdateRow {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .py(rems(0.125))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.foreground)
                            .child(self.group.label.clone()),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(TEXT_XS.size)
                            .text_color(row_tone_color(self.status.kind, theme))
                            .child(self.status.text.clone()),
                    )
                    .when(!self.can_manage_providers, |this| {
                        this.child(
                            div()
                                .text_size(TEXT_XS.size)
                                .text_color(theme.muted_foreground)
                                .child("This connection cannot manage provider accounts."),
                        )
                    }),
            )
            .child(match self.status.kind {
                ProviderUpdateRowStatusKind::Loading => {
                    div().child(Icon::new(IconName::LoaderCircle).size(rems(1.)))
                }
                ProviderUpdateRowStatusKind::Success => div().child(
                    Icon::new(IconName::Check)
                        .size(rems(1.))
                        .color(theme.success),
                ),
                ProviderUpdateRowStatusKind::Failed | ProviderUpdateRowStatusKind::Unchanged => {
                    div().child(
                        Button::new("environment-update-retry")
                            .size(ButtonSize::Xs)
                            .variant(ButtonVariant::Outline)
                            .label("Retry")
                            .disabled(!self.can_manage_providers)
                            .on_click(move |_, window, cx| (self.on_update)(window, cx)),
                    )
                }
                ProviderUpdateRowStatusKind::Idle => div().child(
                    Button::new("environment-update")
                        .size(ButtonSize::Xs)
                        .variant(ButtonVariant::Outline)
                        .label("Update")
                        .disabled(!self.can_manage_providers)
                        .on_click(move |_, window, cx| (self.on_update)(window, cx)),
                ),
            })
    }
}

#[derive(IntoElement)]
pub struct ProviderUpdateEnvironmentRows {
    pub rows: Vec<EnvironmentUpdateRow>,
}

impl RenderOnce for ProviderUpdateEnvironmentRows {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        if self.rows.is_empty() {
            return div().into_any_element();
        }
        div()
            .mt(rems(0.125))
            .flex()
            .flex_col()
            .gap_1()
            .children(self.rows)
            .into_any_element()
    }
}
