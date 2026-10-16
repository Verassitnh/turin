//! Dev hook for screenshots (`TURIN_PALETTE=<step>;<step>…`), in the style of
//! the sidebar's `TURIN_SIDEBAR_*` overrides. Steps run in order, 250ms apart:
//!
//! - `open`: ⌘K
//! - `query:<text>`: types into the input
//! - `down:<n>`: n ArrowDowns
//! - `enter`: Enter on the highlight
//! - `add-project`, `local-folder`, `new-project`, `theme`, `new-thread-in`:
//!   the matching open intent / command
//! - `file-picker`, `content-search`: the other overlay modes

use std::time::Duration;

use gpui_kit::{Context, Window};

use super::command_palette_logic::SearchOverlayMode;
use super::{CommandPalette, CommandPaletteOpenDetail, OpenTarget, palette_source::PaletteContext};

impl CommandPalette {
    /// Runs the steps of `TURIN_PALETTE`, if set.
    pub fn run_demo_steps(&mut self, spec: String, window: &mut Window, cx: &mut Context<Self>) {
        let steps: Vec<String> = spec.split(';').map(|step| step.trim().to_owned()).collect();
        cx.spawn_in(window, async move |this, cx| {
            for step in steps {
                cx.background_executor()
                    .timer(Duration::from_millis(350))
                    .await;
                let outcome =
                    this.update_in(cx, |this, window, cx| this.demo_step(&step, window, cx));
                if outcome.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn demo_step(&mut self, step: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (name, arg) = step.split_once(':').unwrap_or((step, ""));
        match name {
            "open" => self.toggle_mode(SearchOverlayMode::Command, window, cx),
            "query" => {
                let value = arg.to_owned();
                self.set_input_value(&value, window, cx);
                self.handle_query_change(value, window, cx);
            }
            "down" => {
                for _ in 0..arg.parse::<usize>().unwrap_or(1) {
                    self.move_highlight(1);
                }
                cx.notify();
            }
            "enter" => {
                let modifiers = gpui_kit::Modifiers::default();
                self.on_enter(&modifiers, window, cx);
            }
            "add-project" => self.open_with(
                CommandPaletteOpenDetail {
                    open: Some(OpenTarget::AddProject),
                    ..Default::default()
                },
                window,
                cx,
            ),
            "new-thread-in" => self.open_with(
                CommandPaletteOpenDetail {
                    open: Some(OpenTarget::NewThreadIn),
                    ..Default::default()
                },
                window,
                cx,
            ),
            "theme" => self.open_change_theme(window, cx),
            "file-picker" => self.toggle_mode(SearchOverlayMode::Files, window, cx),
            "content-search" => self.toggle_mode(SearchOverlayMode::Content, window, cx),
            "local-folder" => self.start_add_project_browse(window, cx),
            "new-project" => self.start_new_project(window, cx),
            "route" => {
                self.set_context(
                    PaletteContext {
                        active_thread_key: Some(format!("local:{arg}")),
                        active_draft_project_key: None,
                    },
                    cx,
                );
            }
            _ => {}
        }
    }
}
