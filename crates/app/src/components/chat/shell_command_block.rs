//! Port of upstream/apps/web/src/components/chat/ShellCommandBlock.tsx
//!
//! The command a command_execution item ran. Upstream syntax-highlights it with
//! `HighlightedTokens` (the highlighter lives with the S2b media work) once the shell parser
//! loads, and shows plain words until then; the plain layout is what is drawn here, and it is
//! the layout the highlighted version matches so the grammar arriving never reflows the block.

use gpui_kit::{
    App, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _, Window, div,
};

use super::timeline::command_label::with_visible_control_characters;
use crate::state::contracts::js_trim;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;

/// One run of the command's text: a word, which wraps as a unit (`inline-block max-w-full
/// [overflow-wrap:anywhere]`), or the whitespace between words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandPart {
    Word(String),
    Space(String),
}

/// `PlainWords`: `code.split(/(\s+)/u)` alternates words (even) and whitespace (odd); empty
/// parts are dropped.
pub fn plain_words(code: &str) -> Vec<CommandPart> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_space = false;
    for ch in code.chars() {
        let is_space = ch.is_whitespace();
        if is_space != in_space && !current.is_empty() {
            parts.push(if in_space {
                CommandPart::Space(std::mem::take(&mut current))
            } else {
                CommandPart::Word(std::mem::take(&mut current))
            });
        }
        in_space = is_space;
        current.push(ch);
    }
    if !current.is_empty() {
        parts.push(if in_space {
            CommandPart::Space(current)
        } else {
            CommandPart::Word(current)
        });
    }
    parts
}

/// The text shown: trimmed, with control characters made visible.
pub fn shell_command_code(command: &str) -> String {
    with_visible_control_characters(js_trim(command))
}

#[derive(IntoElement)]
pub struct ShellCommandBlock {
    pub command: SharedString,
}

impl RenderOnce for ShellCommandBlock {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let code = shell_command_code(&self.command);
        // `if (!code) return null`.
        if code.is_empty() {
            return div().into_any_element();
        }
        let theme = cx.app_theme();
        // The tool body sets the monospace, pre-wrapped text this sits in: each line is a wrapping
        // row of words, and runs of spaces keep their width.
        let lines = code.split('\n').map(|line| {
            div().flex().flex_wrap().min_h(gpui_kit::rems(1.)).children(
                plain_words(line).into_iter().map(|part| match part {
                    CommandPart::Word(word) => div().max_w_full().child(word).into_any_element(),
                    CommandPart::Space(space) => {
                        div().whitespace_nowrap().child(space).into_any_element()
                    }
                }),
            )
        });
        div()
            .flex()
            .flex_col()
            .text_color(with_alpha(theme.foreground, 0.85))
            .children(lines)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str) -> CommandPart {
        CommandPart::Word(text.into())
    }

    fn space(text: &str) -> CommandPart {
        CommandPart::Space(text.into())
    }

    #[test]
    fn splits_words_from_the_whitespace_between_them() {
        assert_eq!(
            plain_words("git  commit --amend"),
            vec![
                word("git"),
                space("  "),
                word("commit"),
                space(" "),
                word("--amend")
            ]
        );
        assert_eq!(plain_words(" lead"), vec![space(" "), word("lead")]);
        assert_eq!(plain_words(""), Vec::<CommandPart>::new());
    }

    #[test]
    fn a_hyphenated_flag_wraps_as_one_unit() {
        assert_eq!(
            plain_words("rg --exclude 'a b'"),
            vec![
                word("rg"),
                space(" "),
                word("--exclude"),
                space(" "),
                word("'a"),
                space(" "),
                word("b'"),
            ]
        );
    }

    #[test]
    fn trims_and_makes_control_characters_visible() {
        assert_eq!(shell_command_code("  ls \u{1b}[0m \n"), "ls ␛[0m");
        assert_eq!(shell_command_code("   \n "), "");
    }
}
