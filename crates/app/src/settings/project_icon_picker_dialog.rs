//! Port of upstream/apps/web/src/components/settings/ProjectIconPickerDialog.tsx
//! and upstream/apps/web/src/components/settings/ProjectIconPickerDialog.test.tsx.
//!
//! "Choose project icon": a Lucide icon (searchable across the whole set), an
//! emoji, or a monogram, each with a color where it has one.
//!
//! The upstream test renders static markup with mocked UI; here the same
//! expectations (icons first, the Icon type group, the Icon color group) are
//! asserted on [`IconPickerDraft`] and [`ICON_MODES`].

use crate::theme::color::solid_over;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, CursorStyle, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    px, rems,
};
use unicode_normalization::UnicodeNormalization as _;

use super::controls::{FieldSize, text_field};
use super::dialog::{Dialog, dialog_footer, dialog_frame, dialog_header};
use super::project_icon_options::{
    DEFAULT_ICON, PROJECT_EMOJIS, PROJECT_ICON_COLORS, filter_project_icon_names, first_emoji,
};
use super::view::SettingsView;
use crate::components::project_monogram::ProjectMonogram;
use crate::project_icon_colors::project_icon_color;
use crate::project_identity::derive_project_identity;
use crate::state::models::{ProjectIcon, ProjectIconColor};
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_MD, TEXT_SM, TEXT_XS};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, Toggle, ToggleSize, ToggleVariant};

/// `ProjectIconOverride["kind"]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconMode {
    Lucide,
    Emoji,
    Monogram,
}

/// The toggle group, in order: "Icons first" (the upstream test).
pub const ICON_MODES: [(IconMode, &str); 3] = [
    (IconMode::Lucide, "Icons"),
    (IconMode::Emoji, "Emoji"),
    (IconMode::Monogram, "Monogram"),
];

const DEFAULT_EMOJI: &str = "💻";

/// `iconLabel`: `alarm-clock` → `Alarm Clock`.
pub fn icon_label(name: &str) -> String {
    name.split('-')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `monogram.normalize("NFKC").trim().toUpperCase()`.
pub fn normalize_monogram(letters: &str) -> String {
    letters.nfkc().collect::<String>().trim().to_uppercase()
}

/// `isMonogramText`.
pub fn is_valid_monogram(monogram: &str) -> bool {
    turin_contracts::project::ProjectMonogramText::new(monogram).is_ok()
}

/// The dialog's `useState`s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconPickerDraft {
    pub mode: IconMode,
    pub icon_name: String,
    pub color: ProjectIconColor,
    pub letters: String,
    pub emoji: String,
    pub query: String,
    pub custom_emoji: String,
}

impl IconPickerDraft {
    /// The state a freshly opened dialog starts from (`current`, else the
    /// project's automatic identity).
    pub fn new(current: Option<&ProjectIcon>, project_name: &str) -> Self {
        let automatic = derive_project_identity(project_name);
        Self {
            mode: match current {
                Some(ProjectIcon::Emoji(_)) => IconMode::Emoji,
                Some(ProjectIcon::Monogram { .. }) => IconMode::Monogram,
                Some(ProjectIcon::Lucide { .. }) | None => IconMode::Lucide,
            },
            icon_name: match current {
                Some(ProjectIcon::Lucide { name, .. }) => name.clone(),
                _ => DEFAULT_ICON.to_owned(),
            },
            color: match current {
                Some(ProjectIcon::Monogram { color, .. } | ProjectIcon::Lucide { color, .. }) => {
                    *color
                }
                _ => automatic.color,
            },
            letters: match current {
                Some(ProjectIcon::Monogram { text, .. }) => text.clone(),
                _ => automatic.monogram,
            },
            emoji: match current {
                Some(ProjectIcon::Emoji(emoji)) => emoji.clone(),
                _ => DEFAULT_EMOJI.to_owned(),
            },
            query: String::new(),
            custom_emoji: String::new(),
        }
    }

    pub fn monogram(&self) -> String {
        normalize_monogram(&self.letters)
    }

    pub fn valid_monogram(&self) -> bool {
        is_valid_monogram(&self.monogram())
    }

    /// `save`: the chosen icon, or `None` while a monogram is invalid.
    pub fn selection(&self) -> Option<ProjectIcon> {
        match self.mode {
            IconMode::Monogram => {
                let text = self.monogram();
                is_valid_monogram(&text).then_some(ProjectIcon::Monogram {
                    text,
                    color: self.color,
                })
            }
            IconMode::Lucide => Some(ProjectIcon::Lucide {
                name: self.icon_name.clone(),
                color: self.color,
            }),
            IconMode::Emoji => Some(ProjectIcon::Emoji(self.emoji.clone())),
        }
    }

    /// Typing in "Or paste any emoji": the first emoji wins.
    pub fn paste_emoji(&mut self, value: &str) {
        self.custom_emoji = value.to_owned();
        if let Some(emoji) = first_emoji(value) {
            self.emoji = emoji;
        }
    }
}

/// What the picker hands back and who it is for.
#[derive(Debug, Clone, PartialEq)]
pub struct IconPicker {
    pub draft: IconPickerDraft,
    pub project_name: String,
    pub project_ids: Vec<String>,
}

impl SettingsView {
    pub(super) fn open_project_icon_picker(
        &mut self,
        project_ids: Vec<String>,
        project_name: String,
        current: Option<ProjectIcon>,
        cx: &mut Context<Self>,
    ) {
        self.panels.projects.icon_picker = Some(IconPicker {
            draft: IconPickerDraft::new(current.as_ref(), &project_name),
            project_name,
            project_ids,
        });
        self.dialog = Some(Dialog::ProjectIconPicker);
        cx.notify();
    }

    fn update_icon_draft(
        &mut self,
        update: impl FnOnce(&mut IconPickerDraft),
        cx: &mut Context<Self>,
    ) {
        if let Some(picker) = self.panels.projects.icon_picker.as_mut() {
            update(&mut picker.draft);
        }
        cx.notify();
    }

    /// `ProjectIconPickerDialog`.
    pub(super) fn render_project_icon_picker(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = cx.app_theme().clone();
        let picker = self.panels.projects.icon_picker.clone()?;
        let draft = picker.draft.clone();
        let dark = theme.is_dark();
        let selected_color = project_icon_color(draft.color, dark);

        let mode_group = div()
            .flex()
            .w_full()
            .gap(px(2.))
            .p(px(2.))
            .rounded(crate::theme::tokens::RADIUS_LG)
            .bg(solid_over(theme.input, 0.4, theme.background))
            .children(ICON_MODES.iter().map(|(mode, label)| {
                let mode = *mode;
                div().flex_1().child(
                    Toggle::new(SharedString::from(format!("icon-mode-{label}")))
                        .variant(ToggleVariant::Segmented)
                        .size(ToggleSize::Segmented)
                        .label(*label)
                        .pressed(draft.mode == mode)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.update_icon_draft(|draft| draft.mode = mode, cx);
                        })),
                )
            }));

        let color_group = (draft.mode != IconMode::Emoji).then(|| {
            div()
                .child(
                    div()
                        .mb_2()
                        .text_size(TEXT_XS.size)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.muted_foreground)
                        .child("Color"),
                )
                .child(div().flex().flex_wrap().gap(rems(0.375)).children(
                    PROJECT_ICON_COLORS.iter().map(|option| {
                        let value = option.value;
                        let selected = draft.color == value;
                        div()
                            .id(SharedString::from(format!("icon-color-{}", option.label)))
                            .flex()
                            .size_6()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .border_1()
                            .border_color(if selected {
                                with_alpha(theme.foreground, 0.64).into()
                            } else {
                                gpui_kit::transparent_black()
                            })
                            .cursor(CursorStyle::PointingHand)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.update_icon_draft(|draft| draft.color = value, cx);
                            }))
                            .child(
                                div()
                                    .size_4()
                                    .rounded_full()
                                    .bg(project_icon_color(value, dark)),
                            )
                    }),
                ))
        });

        let cell = |id: String, selected: bool| {
            div()
                .id(SharedString::from(id))
                .flex()
                .h(rems(2.6))
                .items_center()
                .justify_center()
                .rounded(RADIUS_MD)
                .border_1()
                .cursor(CursorStyle::PointingHand)
                .border_color(if selected {
                    theme.border
                } else {
                    gpui_kit::transparent_black()
                })
                .when(selected, |this| this.bg(theme.accent))
                .when(!selected, |this| this.hover(|style| style.bg(theme.accent)))
        };

        let body: AnyElement = match draft.mode {
            IconMode::Lucide => {
                let query_input = self.input_live(
                    "icon-picker-query",
                    &draft.query,
                    "Search all Lucide icons",
                    window,
                    cx,
                    |this, text, _, cx| this.update_icon_draft(|draft| draft.query = text, cx),
                );
                let icons = filter_project_icon_names(&draft.query);
                let empty = icons.is_empty();
                let grid = div()
                    .id("icon-picker-grid")
                    .grid()
                    .grid_cols(10)
                    .gap_1()
                    .p(px(2.))
                    .children(icons.into_iter().map(|name| {
                        let selected = draft.icon_name == name;
                        let pick = name.clone();
                        cell(format!("icon-{name}"), selected)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let pick = pick.clone();
                                this.update_icon_draft(move |draft| draft.icon_name = pick, cx);
                            }))
                            .child(
                                Icon::path(format!("icons/{name}.svg"))
                                    .size(rems(1.25))
                                    .color(selected_color),
                            )
                    }));
                div()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(text_field(
                        &query_input,
                        None,
                        FieldSize::Default,
                        false,
                        cx,
                    ))
                    .child(
                        div()
                            .id("icon-picker-scroll")
                            .max_h(rems(16.))
                            .overflow_y_scroll()
                            .child(grid),
                    )
                    .when(empty, |this| {
                        this.child(
                            div()
                                .py_8()
                                .text_center()
                                .text_size(TEXT_SM.size)
                                .text_color(theme.muted_foreground)
                                .child("No icons found."),
                        )
                    })
                    .into_any_element()
            }
            IconMode::Monogram => {
                let input = self.input_live(
                    "icon-picker-monogram",
                    &draft.letters,
                    "",
                    window,
                    cx,
                    |this, text, _, cx| this.update_icon_draft(|draft| draft.letters = text, cx),
                );
                let valid = draft.valid_monogram();
                div()
                    .flex()
                    .items_center()
                    .gap_4()
                    .py_2()
                    .child(
                        ProjectMonogram::new(
                            if valid {
                                draft.monogram()
                            } else {
                                derive_project_identity(&picker.project_name).monogram
                            },
                            selected_color,
                        )
                        .size(rems(3.)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(TEXT_SM.size)
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("Letters"),
                            )
                            .child(div().child(text_field(
                                &input,
                                None,
                                FieldSize::Default,
                                false,
                                cx,
                            )))
                            .child(
                                div()
                                    .text_size(TEXT_XS.size)
                                    .text_color(if valid {
                                        theme.muted_foreground
                                    } else {
                                        theme.destructive
                                    })
                                    .child("One or two letters or numbers."),
                            ),
                    )
                    .into_any_element()
            }
            IconMode::Emoji => {
                let custom = self.input_live(
                    "icon-picker-custom-emoji",
                    &draft.custom_emoji,
                    "Paste an emoji",
                    window,
                    cx,
                    |this, text, _, cx| {
                        this.update_icon_draft(move |draft| draft.paste_emoji(&text), cx)
                    },
                );
                let grid = div()
                    .id("emoji-picker-grid")
                    .grid()
                    .grid_cols(10)
                    .gap_1()
                    .p(px(2.))
                    .children(PROJECT_EMOJIS.iter().map(|(emoji, _label)| {
                        let selected = draft.emoji == *emoji;
                        let pick = (*emoji).to_owned();
                        cell(format!("emoji-{emoji}"), selected)
                            .text_size(rems(1.25))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let pick = pick.clone();
                                this.update_icon_draft(move |draft| draft.emoji = pick, cx);
                            }))
                            .child(*emoji)
                    }));
                div()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(
                        div()
                            .id("emoji-picker-scroll")
                            .max_h(rems(16.))
                            .overflow_y_scroll()
                            .child(grid),
                    )
                    .child(
                        div()
                            .child(
                                div()
                                    .mb_2()
                                    .text_size(TEXT_XS.size)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.muted_foreground)
                                    .child("Or paste any emoji"),
                            )
                            .child(text_field(&custom, None, FieldSize::Default, false, cx)),
                    )
                    .into_any_element()
            }
        };

        let dismiss = self.cb(cx, |this, _, cx| {
            this.dialog = None;
            this.panels.projects.icon_picker = None;
            cx.notify();
        });
        let selection = draft.selection();
        let project_ids = picker.project_ids.clone();
        Some(dialog_frame(
            &theme,
            rems(32.),
            vec![
                dialog_header(
                    "Choose project icon",
                    Some("Choose an icon, emoji, or monogram.".into()),
                    &theme,
                ),
                div()
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .gap_4()
                    .px_6()
                    .pb_6()
                    .child(mode_group)
                    .children(color_group)
                    .child(body)
                    .into_any_element(),
                dialog_footer(
                    &theme,
                    vec![
                        Button::new("icon-picker-cancel")
                            .variant(ButtonVariant::Outline)
                            .size(ButtonSize::Default)
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.dialog = None;
                                this.panels.projects.icon_picker = None;
                                cx.notify();
                            }))
                            .into_any_element(),
                        Button::new("icon-picker-save")
                            .size(ButtonSize::Default)
                            .label("Save icon")
                            .disabled(selection.is_none())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let Some(icon) = selection.clone() else {
                                    return;
                                };
                                this.dialog = None;
                                this.panels.projects.icon_picker = None;
                                this.set_project_icon(project_ids.clone(), None, Some(icon), cx);
                            }))
                            .into_any_element(),
                    ],
                ),
            ],
            dismiss,
        ))
    }

    /// `setProjectIcon`: one icon for every checkout of the project group.
    pub(super) fn set_project_icon(
        &mut self,
        project_ids: Vec<String>,
        favicon_path: Option<String>,
        project_icon: Option<ProjectIcon>,
        cx: &mut Context<Self>,
    ) {
        let source = self.store.read(cx).source();
        if let Err(message) = source.set_project_icon(&project_ids, favicon_path, project_icon, cx)
        {
            self.toast(
                super::view::SettingsToastKind::Error,
                "Failed to update project icon",
                if message.is_empty() {
                    "An error occurred.".to_owned()
                } else {
                    message
                },
                cx,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_on_icons_first_with_the_icon_color_group() {
        assert_eq!(ICON_MODES[0], (IconMode::Lucide, "Icons"));
        assert_eq!(ICON_MODES[1].1, "Emoji");
        let draft = IconPickerDraft::new(None, "Test");
        assert_eq!(draft.mode, IconMode::Lucide);
        assert_eq!(draft.icon_name, "folder-code");
        // An automatic project starts from its derived color and monogram.
        let automatic = derive_project_identity("Test");
        assert_eq!(draft.color, automatic.color);
        assert_eq!(draft.letters, automatic.monogram);
        assert_eq!(draft.emoji, "💻");
    }

    #[test]
    fn starts_from_the_current_icon() {
        let monogram = IconPickerDraft::new(
            Some(&ProjectIcon::Monogram {
                text: "AB".into(),
                color: ProjectIconColor::Teal,
            }),
            "Test",
        );
        assert_eq!(monogram.mode, IconMode::Monogram);
        assert_eq!(monogram.letters, "AB");
        assert_eq!(monogram.color, ProjectIconColor::Teal);

        let emoji = IconPickerDraft::new(Some(&ProjectIcon::Emoji("🚀".into())), "Test");
        assert_eq!(emoji.mode, IconMode::Emoji);
        assert_eq!(emoji.emoji, "🚀");

        let lucide = IconPickerDraft::new(
            Some(&ProjectIcon::Lucide {
                name: "rocket".into(),
                color: ProjectIconColor::Rose,
            }),
            "Test",
        );
        assert_eq!(lucide.mode, IconMode::Lucide);
        assert_eq!(lucide.icon_name, "rocket");
        assert_eq!(lucide.color, ProjectIconColor::Rose);
    }

    #[test]
    fn saves_each_mode_as_its_override() {
        let mut draft = IconPickerDraft::new(None, "Test");
        draft.color = ProjectIconColor::Blue;
        assert_eq!(
            draft.selection(),
            Some(ProjectIcon::Lucide {
                name: "folder-code".into(),
                color: ProjectIconColor::Blue
            })
        );
        draft.mode = IconMode::Emoji;
        draft.emoji = "🚀".into();
        assert_eq!(draft.selection(), Some(ProjectIcon::Emoji("🚀".into())));
        draft.mode = IconMode::Monogram;
        draft.letters = " ab ".into();
        assert_eq!(
            draft.selection(),
            Some(ProjectIcon::Monogram {
                text: "AB".into(),
                color: ProjectIconColor::Blue
            })
        );
    }

    #[test]
    fn refuses_to_save_an_invalid_monogram() {
        let mut draft = IconPickerDraft::new(None, "Test");
        draft.mode = IconMode::Monogram;
        for invalid in ["", "   ", "!!", "a b"] {
            draft.letters = invalid.into();
            assert!(!draft.valid_monogram(), "{invalid:?}");
            assert_eq!(draft.selection(), None, "{invalid:?}");
        }
        draft.letters = "ｔｅ".into();
        // NFKC folds full-width letters.
        assert_eq!(draft.monogram(), "TE");
        assert!(draft.valid_monogram());
    }

    #[test]
    fn pasting_an_emoji_selects_the_first_one() {
        let mut draft = IconPickerDraft::new(None, "Test");
        draft.paste_emoji("🔥 hot");
        assert_eq!(draft.emoji, "🔥");
        draft.paste_emoji("not an emoji");
        assert_eq!(draft.emoji, "🔥");
        assert_eq!(draft.custom_emoji, "not an emoji");
    }

    #[test]
    fn labels_icons_by_title_casing_their_names() {
        assert_eq!(icon_label("alarm-clock"), "Alarm Clock");
        assert_eq!(icon_label("rocket"), "Rocket");
    }
}
