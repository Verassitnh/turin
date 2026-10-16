//! Port of upstream/apps/web/src/components/KeybindingsConfigWarning.tsx
//!
//! The body of the "keybindings.json has a problem" toast: the issue message
//! and an "Open keybindings.json" button that opens the file in the preferred
//! editor, with an error toast when that fails.
//!
//! Upstream reads the editor launcher through `useOpenInPreferredEditor`; here
//! the launch sits behind [`OpenInPreferredEditor`] and the caller shows the
//! toast built by [`open_keybindings_file`]. The `AuthOrchestrationOperateScope`
//! check (`canOpenEditor`) is cut with auth, so only a missing config path or
//! an empty editor list disables the button (PORTING.md Deviations). The toast
//! host is the shell's (T22), so the view takes the click handler.

use std::rc::Rc;

use async_trait::async_trait;
use gpui_kit::{App, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window, div};
use turin_contracts::editor::EditorId;

use crate::ui::{Button, ButtonSize, ButtonVariant};

/// Label of the action button.
pub const OPEN_KEYBINDINGS_BUTTON_LABEL: &str = "Open keybindings.json";
/// Title of the failure toast.
pub const OPEN_KEYBINDINGS_FAILURE_TITLE: &str = "Unable to open keybindings file";
/// Description when the failure carries no message (`error instanceof Error` false).
pub const OPEN_KEYBINDINGS_UNKNOWN_FAILURE: &str = "Unknown error opening file.";

/// Why an open-in-editor attempt failed. `Unknown` stands for a failure that
/// is not an `Error` upstream, which has no message to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenInEditorFailure {
    Error(String),
    Unknown,
}

/// `useOpenInPreferredEditor(environmentId, availableEditors)` as a seam: opens
/// `path` in the preferred editor.
#[async_trait]
pub trait OpenInPreferredEditor: Send + Sync {
    async fn open(&self, path: &str) -> Result<(), OpenInEditorFailure>;
}

/// `stackedThreadToast({ type: "error", title, description })`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorToast {
    pub title: String,
    pub description: String,
}

/// The props of `KeybindingsConfigWarning` minus the environment, which Turin
/// has one of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeybindingsConfigWarningState {
    pub message: String,
    pub config_path: Option<String>,
    pub available_editors: Vec<EditorId>,
}

impl KeybindingsConfigWarningState {
    /// `disabled={!canOpenEditor || !configPath || availableEditors.length === 0}`
    /// without the cut scope check. An empty path is falsy in JS.
    pub fn open_disabled(&self) -> bool {
        self.config_path.as_deref().is_none_or(str::is_empty) || self.available_editors.is_empty()
    }
}

/// The button's `onClick`: nothing without a config path, `None` on success,
/// otherwise the error toast to show.
pub async fn open_keybindings_file(
    opener: &dyn OpenInPreferredEditor,
    config_path: Option<&str>,
) -> Option<ErrorToast> {
    let config_path = config_path.filter(|path| !path.is_empty())?;
    let failure = opener.open(config_path).await.err()?;
    Some(ErrorToast {
        title: OPEN_KEYBINDINGS_FAILURE_TITLE.to_owned(),
        description: match failure {
            OpenInEditorFailure::Error(message) => message,
            OpenInEditorFailure::Unknown => OPEN_KEYBINDINGS_UNKNOWN_FAILURE.to_owned(),
        },
    })
}

type OpenHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// The toast body: the message, then `mt-2 flex justify-end` around an `xs`
/// outline button.
#[derive(IntoElement)]
pub struct KeybindingsConfigWarning {
    state: KeybindingsConfigWarningState,
    on_open: OpenHandler,
}

impl KeybindingsConfigWarning {
    pub fn new(
        state: KeybindingsConfigWarningState,
        on_open: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            state,
            on_open: Rc::new(on_open),
        }
    }
}

impl RenderOnce for KeybindingsConfigWarning {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let on_open = self.on_open;
        div()
            .child(SharedString::from(self.state.message.clone()))
            .child(
                div().mt_2().flex().justify_end().child(
                    Button::new("keybindings-config-warning-open")
                        .size(ButtonSize::Xs)
                        .variant(ButtonVariant::Outline)
                        .label(OPEN_KEYBINDINGS_BUTTON_LABEL)
                        .disabled(self.state.open_disabled())
                        .on_click(move |_, window, cx| on_open(window, cx)),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use futures::executor::block_on;

    use super::*;

    struct FakeOpener {
        opened: Mutex<Vec<String>>,
        result: Result<(), OpenInEditorFailure>,
    }

    impl FakeOpener {
        fn new(result: Result<(), OpenInEditorFailure>) -> Self {
            Self {
                opened: Mutex::new(Vec::new()),
                result,
            }
        }
    }

    #[async_trait]
    impl OpenInPreferredEditor for FakeOpener {
        async fn open(&self, path: &str) -> Result<(), OpenInEditorFailure> {
            self.opened.lock().unwrap().push(path.to_owned());
            self.result.clone()
        }
    }

    fn state(path: Option<&str>, editors: &[EditorId]) -> KeybindingsConfigWarningState {
        KeybindingsConfigWarningState {
            message: "Invalid keybindings".to_owned(),
            config_path: path.map(str::to_owned),
            available_editors: editors.to_vec(),
        }
    }

    #[test]
    fn button_is_disabled_without_a_path_or_editors() {
        assert!(state(None, &[EditorId::Vscode]).open_disabled());
        assert!(state(Some(""), &[EditorId::Vscode]).open_disabled());
        assert!(state(Some("/home/u/keybindings.json"), &[]).open_disabled());
        assert!(!state(Some("/home/u/keybindings.json"), &[EditorId::Vscode]).open_disabled());
    }

    #[test]
    fn opens_the_config_path_and_shows_no_toast_on_success() {
        let opener = FakeOpener::new(Ok(()));
        let toast = block_on(open_keybindings_file(&opener, Some("/k.json")));
        assert_eq!(toast, None);
        assert_eq!(*opener.opened.lock().unwrap(), vec!["/k.json".to_owned()]);
    }

    #[test]
    fn does_nothing_without_a_config_path() {
        let opener = FakeOpener::new(Err(OpenInEditorFailure::Unknown));
        assert_eq!(block_on(open_keybindings_file(&opener, None)), None);
        assert!(opener.opened.lock().unwrap().is_empty());
    }

    #[test]
    fn failure_toast_carries_the_error_message() {
        let opener = FakeOpener::new(Err(OpenInEditorFailure::Error("no editor".to_owned())));
        assert_eq!(
            block_on(open_keybindings_file(&opener, Some("/k.json"))),
            Some(ErrorToast {
                title: "Unable to open keybindings file".to_owned(),
                description: "no editor".to_owned(),
            })
        );
    }

    #[test]
    fn failure_toast_falls_back_when_the_failure_is_not_an_error() {
        let opener = FakeOpener::new(Err(OpenInEditorFailure::Unknown));
        assert_eq!(
            block_on(open_keybindings_file(&opener, Some("/k.json")))
                .map(|toast| toast.description),
            Some("Unknown error opening file.".to_owned())
        );
    }
}
