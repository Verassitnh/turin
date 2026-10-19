//! Live keybindings config issues → stacked toast (body matches
//! [`KeybindingsConfigWarning`]).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::{App, AsyncApp, Subscription};
use turin_contracts::editor::EditorId;
use turin_runtime::keybindings::{KeybindingsConfigState, ServerConfigIssue};

use crate::app_runtime::write::bus::{CommandBus, CommandBusGlobal};
use crate::components::keybindings_config_warning::{
    KeybindingsConfigWarningState, OPEN_KEYBINDINGS_BUTTON_LABEL, OpenInEditorFailure,
    OpenInPreferredEditor, open_keybindings_file,
};
use crate::ui::toast::{ToastActionSpec, ToastOptions, toast_manager};
use crate::ui::toast_logic::ToastType;

struct ShellEditorOpener;

#[async_trait::async_trait]
impl OpenInPreferredEditor for ShellEditorOpener {
    async fn open(&self, path: &str) -> Result<(), OpenInEditorFailure> {
        #[cfg(target_os = "macos")]
        {
            std::process::Command::new("open")
                .arg(path)
                .status()
                .map_err(|error| OpenInEditorFailure::Error(error.to_string()))?;
            return Ok(());
        }
        #[cfg(not(target_os = "macos"))]
        {
            std::process::Command::new("xdg-open")
                .arg(path)
                .status()
                .map_err(|error| OpenInEditorFailure::Error(error.to_string()))?;
            Ok(())
        }
    }
}

fn issue_message(issues: &[ServerConfigIssue]) -> String {
    issues
        .first()
        .map(|issue| match issue {
            ServerConfigIssue::MalformedConfig { message } => message.clone(),
            ServerConfigIssue::InvalidEntry { message, .. } => message.clone(),
        })
        .unwrap_or_else(|| "Invalid keybindings configuration.".to_owned())
}

pub fn start(bus: CommandBus, cx: &mut App) -> Subscription {
    let shown = Arc::new(AtomicBool::new(false));
    let bus = bus.clone();
    cx.spawn(async move |cx| {
        let Ok(commands) = bus.commands() else {
            return;
        };
        let runtime = commands.runtime();
        let config_path = runtime
            .config
            .paths
            .keybindings_config_path
            .to_string_lossy()
            .into_owned();
        let keybindings = Arc::clone(&runtime.keybindings);
        if let Ok(state) = keybindings.snapshot().await {
            maybe_show(&config_path, state, &shown, cx);
        }
        let mut updates = keybindings.subscribe();
        while let Some(state) = updates.recv().await {
            maybe_show(&config_path, state, &shown, cx);
        }
    })
    .detach();
    cx.observe_global::<CommandBusGlobal>(|_| {})
}

fn maybe_show(
    config_path: &str,
    state: KeybindingsConfigState,
    shown: &AtomicBool,
    cx: &mut AsyncApp,
) {
    if state.issues.is_empty() || shown.swap(true, Ordering::SeqCst) {
        return;
    }
    let message = issue_message(&state.issues);
    let state = KeybindingsConfigWarningState {
        message: message.clone(),
        config_path: Some(config_path.to_owned()),
        available_editors: vec![EditorId::Vscode],
    };
    if state.open_disabled() {
        return;
    }
    let path = config_path.to_owned();
    cx.update(|app| {
        toast_manager(app).update(app, |store, cx| {
            store.add(
                ToastOptions::new("Keybindings configuration")
                    .type_(ToastType::Error)
                    .description(message)
                    .timeout(0)
                    .action(
                        ToastActionSpec::new(
                            "keybindings-config-warning-open",
                            OPEN_KEYBINDINGS_BUTTON_LABEL,
                        )
                        .on_click(move |_, cx| {
                            let path = path.clone();
                            cx.spawn(async move |cx| {
                                let opener = ShellEditorOpener;
                                if let Some(toast) =
                                    open_keybindings_file(&opener, Some(&path)).await
                                {
                                    cx.update(|app| {
                                        toast_manager(app).update(app, |store, cx| {
                                            store.add(
                                                ToastOptions::new(toast.title)
                                                    .type_(ToastType::Error)
                                                    .description(toast.description),
                                                cx,
                                            );
                                        });
                                    });
                                }
                            })
                            .detach();
                        }),
                    ),
                cx,
            );
        });
    });
}
