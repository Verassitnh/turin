//! Port of ComposerCodeBlockLanguagePicker.tsx: the searchable language list
//! a fence header opens.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, Context, CursorStyle, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, ScrollHandle,
    StatefulInteractiveElement as _, Styled as _, Window, div, px, rems,
};

use crate::composer::code_languages::{
    CODE_BLOCK_LANGUAGES, CodeBlockLanguage, code_language_entry,
};
use crate::composer::views::chrome::{menu_row, muted_label};
use crate::composer::views::prompt_editor::{
    CommandKey, EditorConfig, PromptEditor, PromptEditorEvent,
};
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::ui::{Icon, IconName};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LanguagePickerEvent {
    Select(String),
    Close,
}

impl EventEmitter<LanguagePickerEvent> for LanguagePickerView {}

pub struct LanguagePickerView {
    search: Entity<PromptEditor>,
    query: String,
    current: String,
    items: Vec<(String, String)>,
    highlighted: usize,
    scroll: ScrollHandle,
}

impl LanguagePickerView {
    pub fn new(language: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let current = code_language_entry(language)
            .map_or_else(|| language.to_owned(), |entry| entry.id.to_owned());
        // A language the list does not carry is offered as its own entry.
        let mut entries: Vec<CodeBlockLanguage> = Vec::new();
        let listed = CODE_BLOCK_LANGUAGES.iter().any(|entry| entry.id == current);
        let mut items: Vec<(String, String)> = Vec::new();
        if !listed {
            items.push((current.clone(), current.clone()));
        }
        entries.extend(CODE_BLOCK_LANGUAGES.iter().copied());
        items.extend(
            entries
                .into_iter()
                .map(|entry| (entry.id.to_owned(), entry.label.to_owned())),
        );
        let highlighted = items.iter().position(|(id, _)| *id == current).unwrap_or(0);
        let search = cx.new(|cx| {
            PromptEditor::new(
                "",
                EditorConfig {
                    placeholder: "Search languages".into(),
                    rich_text: false,
                    literal_text: true,
                    min_height: 1.625,
                    max_height: 1.625,
                    font_size: Some(14.),
                    ..EditorConfig::default()
                },
                Vec::new(),
                cx,
            )
        });
        let weak = cx.weak_entity();
        search.update(cx, |editor, _| {
            editor.set_command_handler(Some(std::rc::Rc::new(move |_, key, window, cx| {
                weak.update(cx, |this, cx| this.on_key(key, window, cx))
                    .unwrap_or(false)
            })));
        });
        cx.subscribe_in(
            &search,
            window,
            |this, _, event: &PromptEditorEvent, _, cx| {
                if let PromptEditorEvent::Changed(snapshot) = event
                    && this.query != snapshot.value
                {
                    this.query = snapshot.value.clone();
                    this.highlighted = 0;
                    cx.notify();
                }
            },
        )
        .detach();
        search.update(cx, |editor, cx| editor.focus_at_end(window, cx));
        Self {
            search,
            query: String::new(),
            current,
            items,
            highlighted,
            scroll: ScrollHandle::new(),
        }
    }

    fn filtered(&self) -> Vec<&(String, String)> {
        let needle = self.query.trim().to_lowercase();
        self.items
            .iter()
            .filter(|(id, label)| {
                needle.is_empty() || label.to_lowercase().contains(&needle) || id.contains(&needle)
            })
            .collect()
    }

    fn on_key(&mut self, key: &CommandKey, _window: &mut Window, cx: &mut Context<Self>) -> bool {
        let count = self.filtered().len();
        match key.key.as_str() {
            "Escape" => {
                cx.emit(LanguagePickerEvent::Close);
                true
            }
            "ArrowDown" | "ArrowUp" if count > 0 => {
                let delta = if key.key == "ArrowDown" { 1 } else { -1 };
                self.highlighted =
                    (self.highlighted as i32 + delta).rem_euclid(count as i32) as usize;
                self.scroll.scroll_to_item(self.highlighted);
                cx.notify();
                true
            }
            "Enter" => {
                if let Some((id, _)) = self
                    .filtered()
                    .get(self.highlighted)
                    .map(|entry| (*entry).clone())
                {
                    self.choose(&id, cx);
                }
                true
            }
            _ => false,
        }
    }

    fn choose(&mut self, id: &str, cx: &mut Context<Self>) {
        if id != self.current {
            cx.emit(LanguagePickerEvent::Select(id.to_owned()));
        } else {
            cx.emit(LanguagePickerEvent::Close);
        }
    }
}

impl Focusable for LanguagePickerView {
    fn focus_handle(&self, cx: &gpui_kit::App) -> FocusHandle {
        self.search.read(cx).focus_handle(cx)
    }
}

impl Render for LanguagePickerView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let filtered: Vec<(String, String)> = self.filtered().into_iter().cloned().collect();
        let rows: Vec<_> = filtered
            .iter()
            .enumerate()
            .map(|(index, (id, label))| {
                let choose_id = id.clone();
                let is_current = *id == self.current;
                menu_row(("language-option", index), index == self.highlighted, cx)
                    .child(
                        Icon::new(IconName::FileText)
                            .size(rems(0.875))
                            .color(theme.muted_foreground),
                    )
                    .child(div().min_w_0().flex_1().truncate().child(label.clone()))
                    .when(is_current, |this| {
                        this.child(Icon::new(IconName::Check).size(rems(1.)))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.choose(&choose_id, cx)))
            })
            .collect();
        div()
            .w(rems(14.))
            .flex()
            .flex_col()
            .child(
                div().px_3().pt(rems(0.625)).child(
                    div()
                        .relative()
                        .border_b_1()
                        .border_color(with_alpha(theme.border, 0.7))
                        .pb(rems(0.375))
                        .child(
                            div().absolute().top(rems(0.375)).left_0().child(
                                Icon::new(IconName::Search)
                                    .color(with_alpha(theme.muted_foreground, 0.55).into()),
                            ),
                        )
                        .child(div().pl(rems(1.25)).child(self.search.clone())),
                ),
            )
            .child(
                div()
                    .id("language-list")
                    .max_h(rems(15.))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .p_1()
                    .cursor(CursorStyle::Arrow)
                    .children(rows)
                    .when(filtered.is_empty(), |this| {
                        this.child(muted_label("No matching languages.", cx).px_2().py_2())
                    }),
            )
            .child(div().h(px(0.)))
    }
}
