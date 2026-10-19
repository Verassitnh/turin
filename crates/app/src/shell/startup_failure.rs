//! Startup failure (the database could not be opened): a dialog that says why
//! and offers recovery, since the sidebar alone would look like a fresh install
//! with no data.
//!
//! - **Try again** reopens the same database (the composition root waits for
//!   [`DatabaseRecovery::Retry`]), for a transient cause such as a held lock.
//! - **Open data folder** reveals the data directory in the file manager.
//! - **Start fresh** moves the old database aside next to itself
//!   (`statev2.sqlite.broken-<time>`, never deleting it) and opens a new one.
//!   It asks for confirmation first.
//!
//! The dialog closes by itself once startup succeeds. A failure the user
//! cannot recover from (the write path would not start) shows the message and
//! the data folder only.

use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Context, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, WeakEntity, Window, div, rems,
};

use crate::app_runtime::write::bus::{CommandBus, DatabaseRecovery};
use crate::theme::ActiveAppTheme;
use crate::ui::dialog::{
    DialogOptions, dialog_footer, dialog_header, dialog_panel, dialog_popup, open_dialog,
};
use crate::ui::{Button, ButtonSize, ButtonVariant};

/// A button the dialog can show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryAction {
    Retry,
    OpenDataFolder,
    StartFresh,
    ConfirmStartFresh,
    CancelStartFresh,
    Close,
}

/// The buttons, in order, for the dialog's current state.
pub fn recovery_actions(
    recoverable: bool,
    confirming_fresh: bool,
    has_data_folder: bool,
) -> Vec<RecoveryAction> {
    if confirming_fresh {
        return vec![
            RecoveryAction::CancelStartFresh,
            RecoveryAction::ConfirmStartFresh,
        ];
    }
    let mut actions = Vec::new();
    if has_data_folder {
        actions.push(RecoveryAction::OpenDataFolder);
    }
    if recoverable {
        actions.push(RecoveryAction::StartFresh);
        actions.push(RecoveryAction::Retry);
    } else {
        actions.push(RecoveryAction::Close);
    }
    actions
}

pub struct StartupFailureDialog {
    bus: CommandBus,
    message: SharedString,
    recoverable: bool,
    confirming_fresh: bool,
    /// A retry or start-fresh is running; the buttons wait for its answer.
    working: bool,
}

impl StartupFailureDialog {
    fn new(bus: CommandBus, message: String) -> Self {
        Self {
            recoverable: bus.failure_is_recoverable(),
            bus,
            message: message.into(),
            confirming_fresh: false,
            working: false,
        }
    }

    /// A new answer from the composition root: another failure message.
    fn set_message(&mut self, message: String, cx: &mut Context<Self>) {
        self.message = message.into();
        self.recoverable = self.bus.failure_is_recoverable();
        self.confirming_fresh = false;
        self.working = false;
        cx.notify();
    }

    fn run(&mut self, action: RecoveryAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            RecoveryAction::Retry => {
                self.working = true;
                self.bus.request_recovery(DatabaseRecovery::Retry);
            }
            RecoveryAction::OpenDataFolder => {
                if let Some(dir) = crate::app_runtime::data_dir::resolve_data_dir() {
                    cx.reveal_path(&dir);
                }
            }
            RecoveryAction::StartFresh => self.confirming_fresh = true,
            RecoveryAction::CancelStartFresh => self.confirming_fresh = false,
            RecoveryAction::ConfirmStartFresh => {
                self.working = true;
                self.confirming_fresh = false;
                self.bus.request_recovery(DatabaseRecovery::StartFresh);
            }
            RecoveryAction::Close => {
                use gpui_kit::component::WindowExt as _;
                window.close_dialog(cx);
            }
        }
        cx.notify();
    }

    fn button(&self, action: RecoveryAction, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (label, variant) = match action {
            RecoveryAction::Retry => ("Try again", ButtonVariant::Default),
            RecoveryAction::OpenDataFolder => ("Open data folder", ButtonVariant::Outline),
            RecoveryAction::StartFresh => ("Start fresh…", ButtonVariant::Outline),
            RecoveryAction::ConfirmStartFresh => {
                ("Move aside and start fresh", ButtonVariant::Destructive)
            }
            RecoveryAction::CancelStartFresh => ("Cancel", ButtonVariant::Outline),
            RecoveryAction::Close => ("Close", ButtonVariant::Outline),
        };
        let waits = self.working && action != RecoveryAction::OpenDataFolder;
        Button::new(SharedString::from(format!("startup-failure-{action:?}")))
            .variant(variant)
            .size(ButtonSize::Sm)
            .label(label)
            .disabled(waits)
            .on_click(cx.listener(move |this, _, window, cx| this.run(action, window, cx)))
    }
}

impl Render for StartupFailureDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let has_folder = crate::app_runtime::data_dir::resolve_data_dir().is_some();
        let description = if self.confirming_fresh {
            "The current database will be moved aside next to itself (not deleted) and Turin will start with an empty one. Your projects and threads will not show until you move it back."
        } else if self.working {
            "Working…"
        } else {
            "Your projects and threads are not shown until this is fixed. Your data has not been deleted."
        };
        let buttons: Vec<_> = recovery_actions(self.recoverable, self.confirming_fresh, has_folder)
            .into_iter()
            .map(|action| self.button(action, cx))
            .collect();
        dialog_popup(window, false)
            .child(dialog_header(
                "Turin could not start",
                Some(description.into()),
                true,
                &theme,
            ))
            .child(
                dialog_panel("startup-failure-panel", true, false).child(
                    div()
                        .text_size(rems(0.8125))
                        .text_color(theme.muted_foreground)
                        .child(self.message.clone()),
                ),
            )
            .child(dialog_footer(&theme, false).children(buttons))
    }
}

/// Opens the dialog when startup fails and keeps it current: a failed retry
/// replaces the message, a successful one closes it.
pub fn start(bus: CommandBus, window: AnyWindowHandle, cx: &mut App) {
    let mut failure = bus.startup_failure();
    cx.spawn(async move |cx| {
        let mut dialog: Option<WeakEntity<StartupFailureDialog>> = None;
        loop {
            let current = failure.borrow_and_update().clone();
            let open = dialog.as_ref().and_then(WeakEntity::upgrade);
            match (current, open) {
                (Some(message), None) => {
                    let bus = bus.clone();
                    dialog = cx.update(|app| {
                        window
                            .update(app, |_, window, cx| {
                                let view = cx.new(|_| StartupFailureDialog::new(bus, message));
                                open_dialog(
                                    window,
                                    cx,
                                    DialogOptions {
                                        max_width: rems(36.),
                                        dismissable: true,
                                        show_close_button: false,
                                    },
                                    view.clone(),
                                );
                                view.downgrade()
                            })
                            .ok()
                    });
                }
                (Some(message), Some(view)) => {
                    cx.update(|app| {
                        view.update(app, |view, cx| view.set_message(message, cx));
                    });
                }
                (None, Some(_)) => {
                    cx.update(|app| {
                        window
                            .update(app, |_, window, cx| {
                                use gpui_kit::component::WindowExt as _;
                                window.close_dialog(cx);
                            })
                            .ok();
                    });
                    dialog = None;
                }
                (None, None) => {}
            }
            if failure.changed().await.is_err() {
                return;
            }
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;
    use RecoveryAction::*;

    #[test]
    fn an_unopenable_database_offers_all_three_recoveries() {
        assert_eq!(
            recovery_actions(true, false, true),
            [OpenDataFolder, StartFresh, Retry]
        );
    }

    #[test]
    fn start_fresh_asks_before_it_moves_anything() {
        assert_eq!(
            recovery_actions(true, true, true),
            [CancelStartFresh, ConfirmStartFresh]
        );
    }

    #[test]
    fn a_failure_that_cannot_be_recovered_only_points_at_the_folder() {
        assert_eq!(
            recovery_actions(false, false, true),
            [OpenDataFolder, Close]
        );
        assert_eq!(recovery_actions(false, false, false), [Close]);
    }

    #[test]
    fn without_a_data_folder_there_is_nothing_to_reveal() {
        assert_eq!(recovery_actions(true, false, false), [StartFresh, Retry]);
    }
}
