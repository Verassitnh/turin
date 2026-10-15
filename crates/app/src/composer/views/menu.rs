//! The `@` / `/` / `$` command menu (ComposerCommandMenu.tsx).

use gpui_kit::{Context, Window};

use crate::composer::editor_state::EditorSnapshot;
use crate::composer::views::chat_composer::ChatComposer;
use crate::composer::views::prompt_editor::CommandKey;

impl ChatComposer {
    pub(super) fn after_prompt_changed(&mut self, _snapshot: &EditorSnapshot, _cx: &mut Context<Self>) {}

    pub(super) fn on_menu_key(&mut self, _snapshot: &EditorSnapshot, _key: &CommandKey, _window: &mut Window, _cx: &mut Context<Self>) -> bool {
        false
    }
}
