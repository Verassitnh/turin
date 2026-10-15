//! Rendering of the diff panel: the toolbar (`headerRow`), the body states,
//! file headers and diff rows as `@pierre/diffs` lays them out
//! (`dist/style.js`), with the app's overrides from `StyledDiffCodeView.tsx`.

use std::ops::Range;

use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClipboardItem, Context, CursorStyle, ElementId, FontWeight, HighlightStyle,
    Hsla, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render, Rgba,
    SharedString, StatefulInteractiveElement as _, Styled as _, StyledText, Window, deferred, div,
    font, list, pattern_slash, px, rems,
};

use super::annotation::CommentInput;
use super::diff_rendering::{DiffCollapseIconTone, get_diff_collapse_icon_tone};
use super::highlight::LineTokens;
use super::menu::{menu_radio_item, menu_sub_trigger, menu_surface};
use super::panel::{
    AUTOMATIC_BASE_REF, BodyState, DiffLayout, DiffPanel, FileEntry, FlatItem, OpenMenu, Scope,
};
use super::pierre::iterate_over_diff::ExpandDirection;
use super::pierre::types::{ChangeType, SelectedLineRange, SelectionSide};
use super::rows::{Cell, DiffRow, ExpandButton, LineKind, Separator, total_lines};
use super::shell::{diff_file_loading_boundary, diff_panel_loading_state};
use super::theme::DiffColors;
use crate::assets::diff_icons;
use crate::components::chat::changed_files_tree::{TurnDiffStat, diff_stat_label};
use crate::components::chat::timestamp_format::{TimestampFormat, format_short_timestamp};
use crate::theme::color::with_alpha;
use crate::theme::tokens::{TEXT_2XS, TEXT_3XS, TEXT_XS};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::{
    Button, ButtonSize, ButtonVariant, Icon, IconName, Toggle, ToggleSize, ToggleVariant, Tooltip,
};

/// `itemMetrics.diffHeaderHeight`.
const HEADER_HEIGHT: f32 = 32.;
/// `lineHeight` (`--diffs-line-height`).
const LINE_HEIGHT: f32 = 20.;
/// `itemMetrics.hunkSeparatorHeight`.
const SEPARATOR_HEIGHT: f32 = 24.;

/// Font metrics of the code surface.
#[derive(Clone)]
struct Metrics {
    /// One `ch` of the code font.
    ch: gpui_kit::Pixels,
    size: gpui_kit::Pixels,
    mono: SharedString,
    sans: SharedString,
}

/// The advance width of `text` in `family` at `size`.
fn measure_text(
    window: &Window,
    text: &str,
    family: &SharedString,
    size: gpui_kit::Pixels,
) -> gpui_kit::Pixels {
    let run = gpui_kit::TextRun {
        len: text.len(),
        font: font(family.clone()),
        color: gpui_kit::black(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .layout_line(text, size, &[run], None)
        .width
}

/// A label that truncates when its row is too narrow. GPUI sizes a
/// `truncate` flex child from its min-content (it collapses to nothing), so
/// the label's own width is measured and given as its flex basis.
fn fitted_text(
    window: &Window,
    text: impl Into<SharedString>,
    family: &SharedString,
    size: gpui_kit::Pixels,
) -> gpui_kit::Div {
    let text: SharedString = text.into();
    let width = measure_text(window, &text, family, size) + px(1.);
    div().min_w_0().w(width).truncate().child(text)
}

fn thousands(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (index, c) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn expand_tabs(
    text: &str,
    tokens: Vec<(Range<usize>, Hsla)>,
) -> (String, Vec<(Range<usize>, Hsla)>) {
    if !text.contains('\t') {
        return (text.to_string(), tokens);
    }
    let mut out = String::with_capacity(text.len());
    let mut map = vec![0usize; text.len() + 1];
    let mut column = 0usize;
    for (index, c) in text.char_indices() {
        map[index] = out.len();
        if c == '\t' {
            let width = 2 - column % 2;
            out.extend(std::iter::repeat_n(' ', width));
            column += width;
        } else {
            out.push(c);
            column += 1;
        }
        for offset in 1..c.len_utf8() {
            map[index + offset] = out.len();
        }
    }
    map[text.len()] = out.len();
    let tokens = tokens
        .into_iter()
        .map(|(range, color)| (map[range.start]..map[range.end], color))
        .collect();
    (out, tokens)
}

impl DiffPanel {
    fn metrics(&self, window: &Window, cx: &App) -> Metrics {
        let theme = cx.app_theme();
        let size = px(theme.fonts.size_code);
        let mono: SharedString = theme.fonts.mono.clone().into();
        let text_system = window.text_system();
        let font_id = text_system.resolve_font(&font(mono.clone()));
        let ch = text_system
            .advance(font_id, size, '0')
            .map(|advance| advance.width)
            .unwrap_or(size * 0.6);
        Metrics {
            ch,
            size,
            mono,
            sans: theme.fonts.sans.clone().into(),
        }
    }

    // ── toolbar ─────────────────────────────────────────────────────────

    fn toolbar_icon_button(
        &self,
        id: &'static str,
        icon: IconName,
        tooltip: impl Into<Tooltip>,
        on_click: impl Fn(&mut DiffPanel, &mut Window, &mut Context<DiffPanel>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let panel = cx.entity();
        Button::new(id)
            .variant(ButtonVariant::Ghost)
            .size(ButtonSize::IconSm)
            .tooltip(tooltip)
            .child(Icon::new(icon).size(rems(0.875)))
            .on_click(move |_, window, cx| {
                panel.update(cx, |panel, cx| on_click(panel, window, cx));
            })
            .into_any_element()
    }

    fn toolbar_toggle(
        &self,
        id: &'static str,
        icon: IconName,
        pressed: bool,
        tooltip: &'static str,
        on_click: impl Fn(&mut DiffPanel, &mut Context<DiffPanel>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let panel = cx.entity();
        Toggle::new(id)
            .variant(ToggleVariant::Ghost)
            .size(ToggleSize::Sm)
            .pressed(pressed)
            .tooltip(tooltip)
            .child(Icon::new(icon).size(rems(0.875)))
            .on_click(move |_, _, cx| {
                panel.update(cx, |panel, cx| on_click(panel, cx));
            })
            .into_any_element()
    }

    /// `headerRow`.
    fn render_toolbar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let panel = cx.entity();
        let compare = self.render_compare(window, cx);
        let sans: SharedString = theme.fonts.sans.clone().into();
        let scope_button = {
            let panel = panel.clone();
            let label = self.derived.selected_scope_label.clone();
            div()
                .relative()
                .flex_shrink_0()
                .child(
                    Button::new("diff-scope")
                        .variant(ButtonVariant::Secondary)
                        .size(ButtonSize::Xs)
                        .pressed(matches!(
                            self.menu,
                            Some(OpenMenu::Scope | OpenMenu::ScopeTurns)
                        ))
                        .child(fitted_text(window, label, &sans, px(12.)))
                        .child(
                            Icon::new(IconName::ChevronDown)
                                .size(rems(0.875))
                                .color(with_alpha(theme.foreground, 0.7).into()),
                        )
                        .on_click(move |_, _, cx| {
                            panel.update(cx, |panel, cx| {
                                panel.menu = match panel.menu {
                                    Some(OpenMenu::Scope | OpenMenu::ScopeTurns) => None,
                                    _ => Some(OpenMenu::Scope),
                                };
                                cx.notify();
                            });
                        }),
                )
                .when(
                    matches!(self.menu, Some(OpenMenu::Scope | OpenMenu::ScopeTurns)),
                    |this| this.child(deferred(self.render_scope_menu(cx)).with_priority(10)),
                )
        };

        let stat = self.derived.has_stat.then(|| {
            div().mr_1().text_size(TEXT_2XS.size).child(diff_stat_label(
                TurnDiffStat {
                    additions: self.derived.stat.additions as u64,
                    deletions: self.derived.stat.deletions as u64,
                },
                cx,
            ))
        });
        let refreshing = self.derived.refreshing;
        let layout = self.settings.layout;
        let all_collapsed = self.all_files_collapsed();
        let has_files = !self.derived.files.is_empty();
        let settings = self.settings;
        let segmented = |id: &'static str,
                         icon: IconName,
                         pressed: bool,
                         tooltip: &'static str,
                         to: DiffLayout,
                         panel: gpui_kit::Entity<DiffPanel>| {
            Toggle::new(id)
                .variant(ToggleVariant::Segmented)
                .size(ToggleSize::Segmented)
                .pressed(pressed)
                .tooltip(tooltip)
                .child(Icon::new(icon).size(rems(0.875)))
                .on_click(move |_, _, cx| {
                    panel.update(cx, |panel, cx| panel.set_layout(to, cx));
                })
        };

        div()
            .id("diff-toolbar")
            .relative()
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_between()
            .gap_2()
            .h(rems(1.75))
            .min_h(rems(1.75))
            .mb_3()
            .px_2()
            .bg(theme.background)
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .flex_1()
                    .items_center()
                    .gap_3()
                    .child(scope_button)
                    .children(compare),
            )
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_1()
                    .children(stat)
                    .when(self.derived.can_refresh, |this| {
                        this.child(self.toolbar_icon_button(
                            "diff-refresh",
                            IconName::RefreshCw,
                            if refreshing {
                                "Refreshing diff…"
                            } else {
                                "Refresh diff"
                            },
                            |panel, _, cx| panel.refresh(cx),
                            cx,
                        ))
                    })
                    .when(has_files, |this| {
                        this.child(self.toolbar_icon_button(
                            "diff-collapse-all",
                            if all_collapsed {
                                IconName::ChevronsUpDown
                            } else {
                                IconName::ChevronsDownUp
                            },
                            if all_collapsed {
                                "Expand all files"
                            } else {
                                "Collapse all files"
                            },
                            |panel, _, cx| panel.toggle_all_files_collapsed(cx),
                            cx,
                        ))
                    })
                    .child(
                        div()
                            .flex()
                            .flex_shrink_0()
                            .child(segmented(
                                "diff-layout-stacked",
                                IconName::Rows3,
                                layout == DiffLayout::Stacked,
                                "Stacked diff view",
                                DiffLayout::Stacked,
                                panel.clone(),
                            ))
                            .child(segmented(
                                "diff-layout-split",
                                IconName::Columns2,
                                layout == DiffLayout::Split,
                                "Split diff view",
                                DiffLayout::Split,
                                panel.clone(),
                            )),
                    )
                    .child(self.toolbar_toggle(
                        "diff-word-wrap",
                        IconName::TextWrap,
                        settings.word_wrap,
                        if settings.word_wrap {
                            "Disable line wrapping"
                        } else {
                            "Enable line wrapping"
                        },
                        move |panel, cx| {
                            let wrap = !panel.settings.word_wrap;
                            panel.set_word_wrap(wrap, cx);
                        },
                        cx,
                    ))
                    .child(self.toolbar_toggle(
                        "diff-ignore-whitespace",
                        IconName::Pilcrow,
                        settings.ignore_whitespace,
                        if settings.ignore_whitespace {
                            "Show whitespace changes"
                        } else {
                            "Hide whitespace changes"
                        },
                        move |panel, cx| {
                            let ignore = !panel.settings.ignore_whitespace;
                            panel.set_ignore_whitespace(ignore, cx);
                        },
                        cx,
                    ))
                    .when(has_files, |this| {
                        this.child(self.toolbar_toggle(
                            "diff-file-tree",
                            IconName::FolderTree,
                            settings.file_tree_open,
                            if settings.file_tree_open {
                                "Hide file tree"
                            } else {
                                "Show file tree"
                            },
                            move |panel, cx| {
                                let open = !panel.settings.file_tree_open;
                                panel.set_file_tree_open(open, cx);
                            },
                            cx,
                        ))
                    }),
            )
            .into_any_element()
    }

    /// `HEAD → base` with the comparison-target combobox.
    fn render_compare(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = cx.app_theme().clone();
        if !matches!(self.derived.scope, Scope::Branch { .. }) {
            return None;
        }
        let source = self.derived.git_source.clone()?;
        let base_ref = source.base_ref.clone()?;
        let head = source.head_ref.clone().unwrap_or_else(|| "HEAD".into());
        let sans: SharedString = theme.fonts.sans.clone().into();
        let panel = cx.entity();
        let open = self.menu == Some(OpenMenu::BaseRef);
        Some(
            div()
                .flex()
                .min_w_0()
                .max_w_full()
                .items_center()
                .gap_2()
                .overflow_hidden()
                .text_size(TEXT_XS.size)
                .text_color(theme.muted_foreground)
                .child(
                    div()
                        .flex()
                        .min_w_0()
                        .items_center()
                        .gap_2()
                        .child(fitted_text(window, head, &sans, px(12.)).max_w(rems(12.)))
                        .child(
                            Icon::new(IconName::ArrowRight)
                                .size(rems(0.875))
                                .color(with_alpha(theme.muted_foreground, 0.7).into()),
                        ),
                )
                .child(
                    div()
                        .relative()
                        .flex_shrink_0()
                        .child(
                            Button::new("diff-base-ref")
                                .variant(ButtonVariant::GhostMuted)
                                .size(ButtonSize::Xs)
                                .pressed(open)
                                .child(
                                    fitted_text(window, base_ref, &sans, px(12.)).max_w(rems(12.)),
                                )
                                .child(
                                    Icon::new(IconName::ChevronDown)
                                        .size(rems(0.875))
                                        .color(with_alpha(theme.muted_foreground, 0.7).into()),
                                )
                                .on_click(move |_, _, cx| {
                                    panel.update(cx, |panel, cx| {
                                        panel.menu = if panel.menu == Some(OpenMenu::BaseRef) {
                                            None
                                        } else {
                                            Some(OpenMenu::BaseRef)
                                        };
                                        panel.base_ref_query.clear();
                                        cx.notify();
                                    });
                                }),
                        )
                        .when(open, |this| {
                            this.child(deferred(self.render_base_ref_menu(cx)).with_priority(10))
                        }),
                )
                .into_any_element(),
        )
    }

    /// The scope dropdown: Changes / Uncommitted / Latest turn and the Turn
    /// submenu.
    fn render_scope_menu(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let panel = cx.entity();
        let selected_value = self.selected_scope_value();
        let selected_turn_value = match &self.derived.scope {
            Scope::Turn { run_id } => format!("turn:{run_id}"),
            _ => String::new(),
        };
        let item =
            |id: &'static str, value: &'static str, label: &'static str, cx: &mut Context<Self>| {
                let panel = panel.clone();
                menu_radio_item(id, selected_value == value, label, cx).on_click(move |_, _, cx| {
                    panel.update(cx, |panel, cx| panel.select_scope(value, cx));
                })
            };
        let turns_open = self.menu == Some(OpenMenu::ScopeTurns);
        let turns: Vec<_> = self.derived.turns.clone();
        let timestamp_format = TimestampFormat::default();
        let mut menu = menu_surface(cx)
            .absolute()
            .top(rems(1.75))
            .left_0()
            .min_w(rems(10.))
            .child(item("diff-scope-branch", "branch", "Changes", cx))
            .child(item("diff-scope-unstaged", "unstaged", "Uncommitted", cx))
            .child(item("diff-scope-latest", "latest", "Latest turn", cx))
            .child({
                let panel = panel.clone();
                menu_sub_trigger("diff-scope-turn", "Turn", turns_open, cx).on_click(
                    move |_, _, cx| {
                        panel.update(cx, |panel, cx| {
                            panel.menu = Some(if panel.menu == Some(OpenMenu::ScopeTurns) {
                                OpenMenu::Scope
                            } else {
                                OpenMenu::ScopeTurns
                            });
                            cx.notify();
                        });
                    },
                )
            });
        if turns_open {
            let theme = cx.app_theme().clone();
            let mut submenu = menu_surface(cx)
                .absolute()
                .top(rems(1.75 * 3. + 0.25))
                .left(rems(10.5))
                .min_w(rems(10.))
                .max_h(rems(18.))
                .overflow_hidden();
            for turn in &turns {
                let value = format!("turn:{}", turn.run_id);
                let count = Derived_turn_count(turn);
                let panel = panel.clone();
                let click_value = value.clone();
                submenu = submenu.child(
                    menu_radio_item(
                        ElementId::Name(format!("diff-scope-{value}").into()),
                        selected_turn_value == value,
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(format!("Turn {count}"))
                            .child(
                                div()
                                    .ml_auto()
                                    .text_size(TEXT_XS.size)
                                    .text_color(theme.muted_foreground)
                                    .child(format_short_timestamp(
                                        &turn.completed_at,
                                        timestamp_format,
                                    )),
                            ),
                        cx,
                    )
                    .on_click(move |_, _, cx| {
                        let value = click_value.clone();
                        panel.update(cx, |panel, cx| panel.select_scope(&value, cx));
                    }),
                );
            }
            menu = menu.child(submenu);
        }
        menu.on_mouse_down_out(cx.listener(|this, _, _, cx| {
            this.menu = None;
            cx.notify();
        }))
        .into_any_element()
    }

    fn selected_scope_value(&self) -> String {
        match &self.derived.scope {
            Scope::Branch { .. } => "branch".into(),
            Scope::Unstaged => "unstaged".into(),
            Scope::Turn { run_id } => {
                if self
                    .derived
                    .turns
                    .first()
                    .is_some_and(|t| &t.run_id == run_id)
                {
                    "latest".into()
                } else {
                    format!("turn:{run_id}")
                }
            }
        }
    }

    /// The comparison-target combobox popup.
    fn render_base_ref_menu(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let panel = cx.entity();
        let selected = match &self.derived.scope {
            Scope::Branch { base_ref } => base_ref.clone(),
            _ => None,
        };
        let query = self.base_ref_query.clone();
        let (choices, matching) = self.base_ref_choices(&query);
        let _ = choices;
        let value_for = |choice: &super::base_ref_choices::BaseRefChoice| -> String {
            match (&selected, &choice.remote) {
                (Some(selected), Some(remote)) if *selected == remote.name => selected.clone(),
                _ => choice
                    .local
                    .as_ref()
                    .map(|r| r.name.clone())
                    .or_else(|| choice.remote.as_ref().map(|r| r.name.clone()))
                    .unwrap_or_else(|| choice.id.clone()),
            }
        };
        let mut list = div()
            .id("diff-base-ref-list")
            .flex()
            .flex_col()
            .max_h(rems(16.))
            .overflow_y_scroll();
        if query.trim().is_empty() {
            let panel = panel.clone();
            list = list.child(
                menu_radio_item("diff-base-auto", selected.is_none(), "Automatic", cx).on_click(
                    move |_, _, cx| {
                        panel.update(cx, |panel, cx| panel.select_base_ref(None, cx));
                    },
                ),
            );
        }
        if matching.is_empty() {
            list = list.child(
                div()
                    .px_3()
                    .py_2()
                    .text_size(TEXT_XS.size)
                    .text_color(theme.muted_foreground)
                    .child("No matching refs."),
            );
        }
        for choice in &matching {
            let value = value_for(choice);
            let has_both = choice.local.is_some() && choice.remote.is_some();
            let use_remote = choice.remote.as_ref().is_some_and(|r| r.name == value);
            let panel = panel.clone();
            let click_value = value.clone();
            list = list.child(
                menu_radio_item(
                    ElementId::Name(format!("diff-base-{}", choice.id).into()),
                    selected.as_deref() == Some(value.as_str()),
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .truncate()
                                .child(choice.label.clone()),
                        )
                        .when(has_both, |this| {
                            this.child(
                                div()
                                    .text_size(TEXT_3XS.size)
                                    .text_color(theme.muted_foreground)
                                    .child(if use_remote { "remote" } else { "local" }),
                            )
                        })
                        .when(!has_both && choice.remote.is_some(), |this| {
                            this.child(
                                Icon::new(IconName::Check)
                                    .size(rems(0.75))
                                    .color(theme.muted_foreground),
                            )
                        }),
                    cx,
                )
                .on_click(move |_, _, cx| {
                    let value = click_value.clone();
                    panel.update(cx, |panel, cx| panel.select_base_ref(Some(&value), cx));
                }),
            );
        }
        menu_surface(cx)
            .absolute()
            .top(rems(1.75))
            .left_0()
            .w(rems(18.))
            .p_0()
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .px_3()
                            .py_2()
                            .border_b_1()
                            .border_color(with_alpha(theme.border, 0.7))
                            .text_size(TEXT_XS.size)
                            .text_color(theme.muted_foreground)
                            .child("Search refs..."),
                    )
                    .child(
                        div()
                            .flex()
                            .px_3()
                            .pt_2()
                            .pb(rems(0.375))
                            .border_b_1()
                            .border_color(with_alpha(theme.border, 0.7))
                            .font_weight(FontWeight::MEDIUM)
                            .text_size(TEXT_3XS.size)
                            .text_color(theme.muted_foreground)
                            .justify_between()
                            .child("BRANCH")
                            .child("REMOTE"),
                    )
                    .child(div().p_1().child(list)),
            )
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.menu = None;
                cx.notify();
            }))
            .into_any_element()
    }

    // ── body ────────────────────────────────────────────────────────────

    fn centered_message(&self, text: impl Into<SharedString>, cx: &App) -> AnyElement {
        let theme = cx.app_theme();
        div()
            .flex()
            .flex_1()
            .items_center()
            .justify_center()
            .px(rems(1.25))
            .text_center()
            .text_size(TEXT_XS.size)
            .text_color(with_alpha(theme.muted_foreground, 0.7))
            .child(text.into())
            .into_any_element()
    }

    fn render_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        match self.derived.body.clone() {
            BodyState::NoThread => {
                self.centered_message("Select a thread to inspect turn diffs.", cx)
            }
            BodyState::NotGit => self.centered_message(
                "Turn diffs are unavailable because this project is not a git repository.",
                cx,
            ),
            BodyState::NoTurns => self.centered_message("No completed turns yet.", cx),
            BodyState::NoFileAccess(message) => self.centered_message(message, cx),
            BodyState::Loading(_) => diff_panel_loading_state(cx),
            BodyState::Message(message) => {
                let error = self.derived.error.clone();
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .bg(theme.background)
                    .children(error.map(|error| {
                        div()
                            .px_3()
                            .mb_2()
                            .text_size(TEXT_2XS.size)
                            .text_color(with_alpha(theme.error, 0.8))
                            .child(error)
                    }))
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .items_center()
                            .justify_center()
                            .px_3()
                            .py_2()
                            .text_size(TEXT_XS.size)
                            .text_color(with_alpha(theme.muted_foreground, 0.7))
                            .child(message),
                    )
                    .into_any_element()
            }
            BodyState::Raw { text, reason } => div()
                .id("diff-raw")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .p_2()
                .gap_2()
                .child(
                    div()
                        .text_size(TEXT_2XS.size)
                        .text_color(with_alpha(theme.muted_foreground, 0.75))
                        .child(reason),
                )
                .child(
                    div()
                        .rounded(px(8.))
                        .border_1()
                        .border_color(with_alpha(theme.border, 0.7))
                        .bg(with_alpha(theme.background, 0.7))
                        .p_3()
                        .font_family(theme.fonts.mono.clone())
                        .text_size(TEXT_2XS.size)
                        .text_color(with_alpha(theme.muted_foreground, 0.9))
                        .when(!self.settings.word_wrap, |this| this.whitespace_nowrap())
                        .child(text),
                )
                .into_any_element(),
            BodyState::Files => self.render_files_body(window, cx),
        }
    }

    fn render_files_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let tree_open = self.settings.file_tree_open && !self.derived.files.is_empty();
        let sticky = self.render_sticky_header(window, cx);
        let banner = self.derived.banner.clone();
        let error = self.derived.error.clone();
        let list_state = self.list.clone();
        let code = div()
            .relative()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(
                list(
                    list_state.clone(),
                    cx.processor(|this, ix, window, cx| this.render_item(ix, window, cx)),
                )
                .size_full(),
            )
            .vertical_scrollbar(&list_state)
            .children(sticky);
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
            .bg(theme.background)
            .children(banner.map(|banner| {
                div()
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(with_alpha(theme.border, 0.7))
                    .bg(with_alpha(theme.muted, 0.4))
                    .px_3()
                    .py(rems(0.375))
                    .text_size(TEXT_2XS.size)
                    .text_color(theme.muted_foreground)
                    .child(banner)
            }))
            .children(error.map(|error| {
                div()
                    .px_3()
                    .mb_2()
                    .text_size(TEXT_2XS.size)
                    .text_color(with_alpha(theme.error, 0.8))
                    .child(error)
            }))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(code)
                    .when(tree_open, |this| {
                        let tree = self.render_file_tree(window, cx);
                        this.child(
                            div()
                                .flex()
                                .flex_shrink_0()
                                .w(rems(16.))
                                .min_w(rems(10.))
                                .max_w(gpui_kit::relative(0.4))
                                .border_l_1()
                                .border_color(with_alpha(theme.border, 0.6))
                                .child(tree),
                        )
                    }),
            )
            .into_any_element()
    }

    /// `stickyHeaders: true`: the header of the file at the top stays put.
    fn render_sticky_header(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let top = self.list.logical_scroll_top();
        let item = *self.items.get(top.item_ix)?;
        let file_index = match item {
            FlatItem::Header(file) if top.offset_in_item > px(0.) => file,
            FlatItem::Row(file, _) | FlatItem::Gap(file) | FlatItem::Annotation(file, _) => file,
            _ => return None,
        };
        if matches!(item, FlatItem::Gap(_)) {
            return None;
        }
        let header = self.render_file_header(file_index, true, window, cx);
        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .right(px(0.))
                .child(header)
                .into_any_element(),
        )
    }

    // ── list items ──────────────────────────────────────────────────────

    pub(super) fn render_item(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(item) = self.items.get(ix).copied() else {
            return div().into_any_element();
        };
        let theme = cx.app_theme().clone();
        match item {
            FlatItem::Header(file) => self.render_file_header(file, false, window, cx),
            FlatItem::Row(file, row) => self.render_row(file, row, window, cx),
            FlatItem::Annotation(file, row) => self.render_annotation(file, row, window, cx),
            FlatItem::Gap(_) => div()
                .h(px(8.))
                .w_full()
                .bg(theme.code_background)
                .into_any_element(),
            FlatItem::LoadingBoundary => {
                let remaining = self
                    .derived
                    .files
                    .len()
                    .saturating_sub(self.derived.settled_count);
                let panel = cx.entity();
                cx.defer(move |cx| {
                    panel.update(cx, |panel, cx| panel.load_next_files(cx));
                });
                diff_file_loading_boundary(remaining, cx)
            }
        }
    }

    /// The pierre file header with the app's overrides
    /// (`min-height: 32px`, 12px sans, `padding-inline: 8px 12px`).
    fn render_file_header(
        &mut self,
        file_index: usize,
        sticky: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let colors = DiffColors::resolve(&theme);
        let metrics = self.metrics(window, cx);
        let Some(entry) = self.derived.files.get(file_index).cloned() else {
            return div().into_any_element();
        };
        let file = entry.file.clone();
        let collapsed = self.collapsed_keys.contains(&entry.key) || entry.pending;
        let tone = match get_diff_collapse_icon_tone(&file) {
            DiffCollapseIconTone::Addition => colors.addition_base,
            DiffCollapseIconTone::Deletion => colors.deletion_base,
            DiffCollapseIconTone::Modified => colors.modified_base,
            DiffCollapseIconTone::Muted => with_alpha(theme.muted_foreground, 0.8).into(),
        };
        let (change_icon, change_color) = match file.r#type {
            ChangeType::New => (diff_icons::SYMBOL_ADDED, colors.addition_base),
            ChangeType::Deleted => (diff_icons::SYMBOL_DELETED, colors.deletion_base),
            ChangeType::Change => (diff_icons::SYMBOL_MODIFIED, colors.modified_base),
            ChangeType::RenamePure | ChangeType::RenameChanged => {
                (diff_icons::SYMBOL_MOVED, colors.modified_base)
            }
        };
        let panel = cx.entity();
        let key = entry.key.clone();
        let toggle_key = key.clone();
        let toggle_panel = panel.clone();
        let header_panel = panel.clone();
        let header_key = key.clone();
        let path = entry.path.clone();
        let title_panel = panel.clone();
        let title_path = path.clone();
        let copy_panel = panel.clone();
        let copy_path = path.clone();
        let copied = self.copied_path.as_deref() == Some(path.as_str());
        let group: SharedString = format!("diff-header-{}-{sticky}", file_index).into();
        let (additions, deletions) = match &entry.stat {
            Some(stat) => (stat.additions, stat.deletions),
            None => file.hunks.iter().fold((0, 0), |(a, d), hunk| {
                (a + hunk.addition_lines, d + hunk.deletion_lines)
            }),
        };
        let metadata: AnyElement = if let Some(stat) = &entry.stat {
            div()
                .text_size(TEXT_3XS.size)
                .child(diff_stat_label(
                    TurnDiffStat {
                        additions: stat.additions as u64,
                        deletions: stat.deletions as u64,
                    },
                    cx,
                ))
                .into_any_element()
        } else {
            div()
                .flex()
                .items_center()
                .gap(metrics.ch)
                .font_family(metrics.mono.clone())
                .text_size(px(11.))
                .when(deletions > 0 || additions == 0, |this| {
                    this.child(
                        div()
                            .text_color(colors.deletion_base)
                            .child(format!("-{}", thousands(deletions))),
                    )
                })
                .when(additions > 0 || deletions == 0, |this| {
                    this.child(
                        div()
                            .text_color(colors.addition_base)
                            .child(format!("+{}", thousands(additions))),
                    )
                })
                .into_any_element()
        };
        let status_button = (entry.stat.is_some() && (entry.error || entry.truncated)).then(|| {
            let retry_panel = panel.clone();
            let error = entry.error;
            Button::new(ElementId::Name(
                format!("diff-status-{}-{sticky}", file_index).into(),
            ))
            .variant(ButtonVariant::GhostMuted)
            .size(ButtonSize::IconMicro)
            .tooltip(if error {
                "Retry loading diff"
            } else {
                "This file is too large to show in full. Counts include all changes."
            })
            .child(
                Icon::new(if error {
                    IconName::RotateCw
                } else {
                    IconName::Info
                })
                .size(rems(0.75)),
            )
            .on_click(move |_, _, cx| {
                if error {
                    retry_panel.update(cx, |panel, cx| panel.reload(cx));
                }
            })
        });
        div()
            .id(ElementId::Name(
                format!("diff-header-{}-{sticky}", file_index).into(),
            ))
            .group(group.clone())
            .relative()
            .flex()
            .w_full()
            .min_h(px(HEADER_HEIGHT))
            .items_center()
            .justify_between()
            .gap(px(8.))
            .pl(px(8.))
            .pr(px(12.))
            .py(px(6.))
            .bg(colors.bg)
            .font_family(metrics.sans.clone())
            .text_size(px(12.))
            .line_height(px(16.))
            .text_color(colors.fg)
            .cursor(CursorStyle::PointingHand)
            // Hover edge cue: an inset 3px bar (`box-shadow: inset 3px 0`).
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left_0()
                    .w(px(3.))
                    .bg(colors.header_hover_edge)
                    .invisible()
                    .group_hover(group, |style| style.visible()),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        Button::new(ElementId::Name(
                            format!("diff-collapse-{}-{sticky}", file_index).into(),
                        ))
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::IconMicro)
                        .ml(px(-2.))
                        .disabled(entry.pending)
                        .tooltip(if collapsed {
                            "Expand diff"
                        } else {
                            "Collapse diff"
                        })
                        .child(
                            Icon::new(if collapsed {
                                IconName::ChevronRight
                            } else {
                                IconName::ChevronDown
                            })
                            .size(rems(1.))
                            .color(tone),
                        )
                        .on_click(move |_, _, cx| {
                            let key = toggle_key.clone();
                            toggle_panel
                                .update(cx, |panel, cx| panel.toggle_file_collapsed(&key, cx));
                        }),
                    )
                    .child(Icon::path(change_icon).size(rems(1.)).color(change_color))
                    .when_some(file.prev_name.clone(), |this, prev| {
                        this.child(fitted_text(window, prev, &metrics.sans, px(12.)).opacity(0.7))
                            .child(
                                Icon::path(diff_icons::ARROW_RIGHT_SHORT)
                                    .size(rems(1.))
                                    .color(colors.fg),
                            )
                    })
                    .child(
                        div()
                            .id(ElementId::Name(
                                format!("diff-title-{}-{sticky}", file_index).into(),
                            ))
                            .min_w_0()
                            .w(measure_text(window, &path, &metrics.sans, px(12.)) + px(1.))
                            .truncate()
                            .cursor(CursorStyle::PointingHand)
                            .hover(|style| {
                                style.text_color(crate::theme::color::color_mix(
                                    crate::theme::color::MixSpace::Srgb,
                                    colors.fg,
                                    0.84,
                                    theme.primary,
                                ))
                            })
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(move |_, _, cx| {
                                let path = title_path.clone();
                                title_panel.update(cx, |panel, cx| panel.open_diff_file(&path, cx));
                            })
                            .child(path.clone()),
                    )
                    .child(
                        Button::new(ElementId::Name(
                            format!("diff-copy-{}-{sticky}", file_index).into(),
                        ))
                        .variant(ButtonVariant::GhostMuted)
                        .size(ButtonSize::IconMicro)
                        .tooltip(if copied { "Copied" } else { "Copy path" })
                        .child(
                            Icon::new(if copied {
                                IconName::Check
                            } else {
                                IconName::Copy
                            })
                            .size(rems(0.75))
                            .when(copied, |icon| icon.color(theme.success_foreground)),
                        )
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy_path.clone()));
                            let path = copy_path.clone();
                            let panel = copy_panel.clone();
                            panel.update(cx, |panel, cx| {
                                panel.copied_path = Some(path.clone());
                                cx.notify();
                                cx.spawn(async move |this, cx| {
                                    cx.background_executor()
                                        .timer(std::time::Duration::from_millis(2000))
                                        .await;
                                    this.update(cx, |panel, cx| {
                                        if panel.copied_path.as_deref() == Some(path.as_str()) {
                                            panel.copied_path = None;
                                            cx.notify();
                                        }
                                    })
                                    .ok();
                                })
                                .detach();
                            });
                        }),
                    )
                    .children(status_button),
            )
            .child(div().flex_shrink_0().child(metadata))
            .on_click(move |_, _, cx| {
                let key = header_key.clone();
                header_panel.update(cx, |panel, cx| panel.toggle_file_collapsed(&key, cx));
            })
            .into_any_element()
    }

    // ── rows ────────────────────────────────────────────────────────────

    fn render_row(
        &mut self,
        file_index: usize,
        row_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let colors = DiffColors::resolve(&theme);
        let metrics = self.metrics(window, cx);
        let Some(entry) = self.derived.files.get(file_index).cloned() else {
            return div().into_any_element();
        };
        let Some(row) = self
            .file_rows
            .get(file_index)
            .and_then(|rows| rows.get(row_index))
            .copied()
        else {
            return div().into_any_element();
        };
        let digits = total_lines(&entry.file).to_string().len();
        let highlights = self.highlights(&entry, theme.is_dark());
        let ctx = RowCtx {
            colors,
            metrics,
            digits,
            wrap: self.settings.word_wrap,
            entry: entry.clone(),
            file_index,
        };
        match row {
            DiffRow::Separator(separator) => self.render_separator(&ctx, separator, cx),
            DiffRow::Line(cell) => {
                let line = self.cell_line(&ctx, &highlights, cell, cx);
                div()
                    .flex()
                    .w_full()
                    .bg(ctx.colors.bg)
                    .child(line)
                    .into_any_element()
            }
            DiffRow::Split { left, right } => {
                let left_el = match left {
                    Some(cell) => self.cell_line(&ctx, &highlights, cell, cx),
                    None => self.buffer_cell(&ctx),
                };
                let right_el = match right {
                    Some(cell) => self.cell_line(&ctx, &highlights, cell, cx),
                    None => self.buffer_cell(&ctx),
                };
                div()
                    .flex()
                    .w_full()
                    .bg(ctx.colors.bg)
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .border_r_1()
                            .border_color(ctx.colors.bg)
                            .child(left_el),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .border_l_1()
                            .border_color(ctx.colors.bg)
                            .child(right_el),
                    )
                    .into_any_element()
            }
            DiffRow::NoNewline { left, right, kind } => {
                let marker = |ctx: &RowCtx| self.no_newline_cell(ctx, kind);
                match self.settings.layout {
                    DiffLayout::Stacked => div()
                        .flex()
                        .w_full()
                        .bg(ctx.colors.bg)
                        .child(marker(&ctx))
                        .into_any_element(),
                    DiffLayout::Split => div()
                        .flex()
                        .w_full()
                        .bg(ctx.colors.bg)
                        .child(div().flex().flex_1().min_w_0().child(if left {
                            marker(&ctx)
                        } else {
                            self.buffer_cell(&ctx)
                        }))
                        .child(div().flex().flex_1().min_w_0().child(if right {
                            marker(&ctx)
                        } else {
                            self.buffer_cell(&ctx)
                        }))
                        .into_any_element(),
                }
            }
        }
    }

    fn gutter_width(&self, ctx: &RowCtx) -> gpui_kit::Pixels {
        gutter_width_for(ctx.metrics.ch, ctx.digits)
    }

    /// An empty split cell: hatched content and a context-gutter cell.
    fn buffer_cell(&self, ctx: &RowCtx) -> AnyElement {
        let gutter = self.gutter_width(ctx);
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .h(px(LINE_HEIGHT))
            .child(
                div()
                    .flex_shrink_0()
                    .w(gutter)
                    .h_full()
                    .bg(ctx.colors.bg_context_gutter)
                    .border_r(px(2.))
                    .border_color(ctx.colors.bg),
            )
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .bg(pattern_slash(ctx.colors.bg_buffer, 1.4, 5.66)),
            )
            .into_any_element()
    }

    fn no_newline_cell(&self, ctx: &RowCtx, kind: LineKind) -> AnyElement {
        let (line_bg, number_bg) = self.kind_backgrounds(&ctx.colors, kind, false);
        let gutter = self.gutter_width(ctx);
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .h(px(LINE_HEIGHT))
            .child(
                div()
                    .flex_shrink_0()
                    .w(gutter)
                    .h_full()
                    .bg(number_bg)
                    .border_r(px(2.))
                    .border_color(ctx.colors.bg),
            )
            .child(
                div()
                    .flex_1()
                    .px(ctx.metrics.ch)
                    .bg(line_bg)
                    .opacity(0.6)
                    .font_family(ctx.metrics.mono.clone())
                    .text_size(ctx.metrics.size)
                    .line_height(px(LINE_HEIGHT))
                    .text_color(ctx.colors.fg)
                    .child("\\ No newline at end of file"),
            )
            .into_any_element()
    }

    fn kind_backgrounds(
        &self,
        colors: &DiffColors,
        kind: LineKind,
        selected: bool,
    ) -> (Hsla, Hsla) {
        match (kind, selected) {
            (_, true) => (colors.selected_line_bg, colors.selected_number_bg),
            (LineKind::Addition, _) => (colors.line_bg_addition, colors.number_bg_addition),
            (LineKind::Deletion, _) => (colors.line_bg_deletion, colors.number_bg_deletion),
            (LineKind::Context, _) => (colors.bg, colors.bg),
        }
    }

    fn is_selected(&self, ctx: &RowCtx, cell: Cell) -> bool {
        let Some(selection) = &self.selection else {
            return false;
        };
        if selection.file_key != ctx.entry.key {
            return false;
        }
        let range = selection.range;
        let (low, high) = (range.start.min(range.end), range.start.max(range.end));
        if cell.number < low || cell.number > high {
            return false;
        }
        match cell.kind {
            LineKind::Context => true,
            _ => {
                let start_side = range.side.unwrap_or(SelectionSide::Additions);
                let end_side = range.end_side.unwrap_or(start_side);
                cell.side == start_side || cell.side == end_side
            }
        }
    }

    /// One side of a row: gutter number cell plus the code cell.
    fn cell_line(
        &mut self,
        ctx: &RowCtx,
        highlights: &super::highlight::FileHighlights,
        cell: Cell,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let colors = ctx.colors;
        let selected = self.is_selected(ctx, cell);
        let (line_bg, number_bg) = self.kind_backgrounds(&colors, cell.kind, selected);
        let number_fg = match cell.kind {
            LineKind::Addition => colors.addition_base,
            LineKind::Deletion => colors.deletion_base,
            LineKind::Context => colors.fg_number,
        };
        let (lines, side_highlights) = match cell.side {
            SelectionSide::Additions => (&ctx.entry.file.addition_lines, &highlights.additions),
            SelectionSide::Deletions => (&ctx.entry.file.deletion_lines, &highlights.deletions),
        };
        let raw = lines.get(cell.line_index).map_or("", String::as_str);
        let text = raw.strip_suffix('\n').unwrap_or(raw);
        let text = text.strip_suffix('\r').unwrap_or(text);
        let tokens = side_highlights.line(cell.line_index, text.len());
        let (shown, runs) = expand_tabs(text, tokens.tokens.clone());
        let code = code_text(&shown, &runs, tokens.foreground, &ctx.metrics, ctx.wrap);
        let gutter = self.gutter_width(ctx);
        let bar_color: Option<Hsla> = match cell.kind {
            LineKind::Addition => Some(colors.addition_base),
            LineKind::Deletion => Some(
                crate::theme::color::color_mix(
                    crate::theme::color::MixSpace::Srgb,
                    colors.deletion_base,
                    0.5,
                    Rgba::from(number_bg),
                )
                .into(),
            ),
            LineKind::Context => None,
        };
        let panel = cx.entity();
        let select_panel = panel.clone();
        let plus_panel = panel.clone();
        let file_index = ctx.file_index;
        let file_key = ctx.entry.key.clone();
        let group: SharedString = format!(
            "diff-line-{}-{:?}-{}-{}",
            file_index, cell.side, cell.number, cell.line_index
        )
        .into();
        let plus_group = group.clone();
        let hover_color = theme.primary;
        let _ = hover_color;
        div()
            .group(group)
            .flex()
            .flex_1()
            .min_w_0()
            .min_h(px(LINE_HEIGHT))
            .child(
                div()
                    .id(ElementId::Name(
                        format!(
                            "diff-num-{}-{:?}-{}-{}",
                            file_index, cell.side, cell.number, cell.line_index
                        )
                        .into(),
                    ))
                    .relative()
                    .flex_shrink_0()
                    .w(gutter)
                    .pl(ctx.metrics.ch * 2.)
                    .pr(ctx.metrics.ch)
                    .bg(number_bg)
                    .border_r(px(2.))
                    .border_color(colors.bg)
                    .font_family(ctx.metrics.mono.clone())
                    .text_size(ctx.metrics.size)
                    .line_height(px(LINE_HEIGHT))
                    .text_color(if selected { colors.fg } else { number_fg })
                    .text_right()
                    .whitespace_nowrap()
                    .cursor(CursorStyle::PointingHand)
                    .children(bar_color.map(|color| {
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left_0()
                            .w(px(4.))
                            .bg(color)
                    }))
                    .child(cell.number.to_string())
                    .child(
                        div()
                            .id(ElementId::Name(
                                format!(
                                    "diff-plus-{}-{:?}-{}-{}",
                                    file_index, cell.side, cell.number, cell.line_index
                                )
                                .into(),
                            ))
                            .absolute()
                            .top_0()
                            .right_0()
                            .size(px(LINE_HEIGHT))
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(colors.modified_base)
                            .text_color(colors.bg)
                            .opacity(0.)
                            .group_hover(plus_group, |style| style.opacity(1.))
                            .child(Icon::path(diff_icons::PLUS).size(px_rems(LINE_HEIGHT - 4.)))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click({
                                let key = file_key.clone();
                                move |_, _, cx| {
                                    let key = key.clone();
                                    plus_panel.update(cx, |panel, cx| {
                                        panel.begin_comment_at(&key, file_index, cell, cx);
                                    });
                                }
                            }),
                    )
                    .on_click(move |event, _, cx| {
                        let shift = event.modifiers().shift;
                        let key = file_key.clone();
                        select_panel.update(cx, |panel, cx| {
                            panel.select_line(&key, cell, shift, cx);
                        });
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .px(ctx.metrics.ch)
                    .bg(line_bg)
                    .overflow_hidden()
                    .font_family(ctx.metrics.mono.clone())
                    .text_size(ctx.metrics.size)
                    .line_height(px(LINE_HEIGHT))
                    .text_color(tokens.foreground)
                    .when(!ctx.wrap, |this| this.whitespace_nowrap())
                    .child(code),
            )
            .into_any_element()
    }

    /// `createSeparator` (`line-info`): a 24px pill with the expand buttons
    /// and "N unmodified lines".
    fn render_separator(
        &mut self,
        ctx: &RowCtx,
        separator: Separator,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = ctx.colors;
        let panel = cx.entity();
        let buttons = separator.buttons();
        let file_index = ctx.file_index;
        let hunk_index = separator.hunk_index;
        let expandable = separator.expandable;
        let label = separator.label();
        let multi = buttons.len() > 1;
        let hover_bg = colors.bg_separator_hover;
        let fg = colors.fg_separator;
        let fg_hover = colors.fg;
        let button_column: AnyElement = {
            let mut column = div()
                .flex()
                .flex_col()
                .flex_shrink_0()
                .w(px(34.))
                .h_full()
                .rounded_l(px(6.));
            for (index, button) in buttons.iter().copied().enumerate() {
                let panel = panel.clone();
                let direction = match button {
                    ExpandButton::Up => ExpandDirection::Up,
                    ExpandButton::Down => ExpandDirection::Down,
                    ExpandButton::Both => ExpandDirection::Both,
                };
                let icon = match button {
                    // `[data-expand-down] [data-icon] { transform: scaleY(-1) }`.
                    ExpandButton::Down => diff_icons::EXPAND_UP,
                    ExpandButton::Up => diff_icons::EXPAND,
                    ExpandButton::Both => diff_icons::EXPAND_ALL,
                };
                column = column.child(
                    div()
                        .id(ElementId::Name(
                            format!("diff-expand-{file_index}-{hunk_index}-{index}").into(),
                        ))
                        .flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .bg(colors.bg_separator)
                        .text_color(fg)
                        .border_r(px(2.))
                        .border_color(colors.bg)
                        .when(multi && index == 0, |this| this.border_b(px(1.)))
                        .when(multi && index == 1, |this| this.border_t(px(1.)))
                        .cursor(CursorStyle::PointingHand)
                        .hover(move |style| style.text_color(fg_hover))
                        .child(Icon::path(icon).size(px_rems(16.)))
                        .on_click(move |event, _, cx| {
                            let all = event.modifiers().shift;
                            panel.update(cx, |panel, cx| {
                                panel.expand_hunk(file_index, hunk_index, direction, all, cx)
                            });
                        }),
                );
            }
            column.into_any_element()
        };
        let text_panel = panel.clone();
        let chunked = separator.chunked && expandable;
        let expand_all_panel = panel.clone();
        let text = div()
            .id(ElementId::Name(
                format!("diff-sep-text-{file_index}-{hunk_index}").into(),
            ))
            .flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .items_center()
            .px(ctx.metrics.ch)
            .bg(colors.bg_separator)
            .text_color(fg)
            .font_family(ctx.metrics.sans.clone())
            .text_size(px(11.))
            .overflow_hidden()
            .when(!separator.expandable || buttons.is_empty(), |this| {
                this.rounded(px(6.))
            })
            .when(separator.expandable && !buttons.is_empty(), |this| {
                this.rounded_r(px(6.))
            })
            .when(expandable, |this| {
                this.cursor(CursorStyle::PointingHand)
                    .hover(move |style| style.bg(hover_bg).text_color(fg_hover))
                    .on_click(move |event, _, cx| {
                        let all = event.modifiers().shift;
                        text_panel.update(cx, |panel, cx| {
                            panel.expand_hunk(
                                file_index,
                                hunk_index,
                                ExpandDirection::Both,
                                all,
                                cx,
                            )
                        });
                    })
            })
            .child(div().truncate().child(label));
        div()
            .flex()
            .w_full()
            .h(px(SEPARATOR_HEIGHT))
            .bg(colors.bg)
            .px(px(8.))
            .child(
                div()
                    .flex()
                    .w_full()
                    .h_full()
                    .when(expandable && !buttons.is_empty(), |this| {
                        this.child(button_column)
                    })
                    .child(text)
                    .when(chunked, |this| {
                        this.child(
                            div()
                                .id(ElementId::Name(
                                    format!("diff-expand-all-{file_index}-{hunk_index}").into(),
                                ))
                                .flex()
                                .items_center()
                                .px(ctx.metrics.ch)
                                .bg(colors.bg_separator)
                                .text_color(fg)
                                .font_family(ctx.metrics.sans.clone())
                                .text_size(px(11.))
                                .rounded_r(px(6.))
                                .cursor(CursorStyle::PointingHand)
                                .hover(move |style| style.text_color(fg_hover))
                                .child("Expand all")
                                .on_click(move |_, _, cx| {
                                    expand_all_panel.update(cx, |panel, cx| {
                                        panel.expand_hunk(
                                            file_index,
                                            hunk_index,
                                            ExpandDirection::Both,
                                            true,
                                            cx,
                                        )
                                    });
                                }),
                        )
                    }),
            )
            .into_any_element()
    }

    // ── comments ────────────────────────────────────────────────────────

    /// `renderAnnotation`: the saved comments and the draft after a row.
    fn render_annotation(
        &mut self,
        file_index: usize,
        row_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let colors = DiffColors::resolve(&theme);
        let Some(entry) = self.derived.files.get(file_index).cloned() else {
            return div().into_any_element();
        };
        let Some(rows) = self.file_rows.get(file_index).cloned() else {
            return div().into_any_element();
        };
        let mut sections: Vec<AnyElement> = Vec::new();
        let mut has_draft = false;
        let comments = self.comments.clone();
        for comment in &comments {
            if comment.section_id != self.derived.review_section_id
                || comment.file_path != entry.path
                || comment.fence_language.as_deref().unwrap_or("diff") != "diff"
            {
                continue;
            }
            let Some(range) = super::review_comment_context::restore_diff_review_comment_range(
                &entry.file,
                comment,
            ) else {
                continue;
            };
            let side = super::panel::annotation_side(&range);
            if super::panel::row_index_for_line(&rows, side, range.end) != Some(row_index) {
                continue;
            }
            sections.push(self.saved_comment(comment, &theme, cx));
        }
        if let Some(draft) = self.draft.clone()
            && draft.file_key == entry.key
            && super::panel::row_index_for_line(
                &rows,
                draft.annotation.side,
                draft.annotation.line_number,
            ) == Some(row_index)
        {
            has_draft = true;
            sections.push(self.draft_comment(&draft.range_label, window, cx));
        }
        div()
            .w_full()
            .bg(colors.bg_context)
            .font_family(theme.fonts.sans.clone())
            .text_color(theme.foreground)
            .when(has_draft, |this| this.py_1())
            .when(!has_draft, |this| {
                this.border_y_1()
                    .border_color(with_alpha(theme.border, 0.3))
            })
            .children(sections)
            .into_any_element()
    }

    /// `DiffCommentAnnotation` kind `comment`.
    fn saved_comment(
        &self,
        comment: &super::review_comment_context::ReviewCommentContext,
        theme: &AppTheme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let panel = cx.entity();
        let id = comment.id.clone();
        let group: SharedString = format!("diff-comment-{}", comment.id).into();
        div()
            .group(group.clone())
            .flex()
            .min_w_0()
            .items_start()
            .gap(rems(0.625))
            .border_l(px(2.))
            .border_color(with_alpha(theme.primary, 0.55))
            .bg(with_alpha(theme.primary, 0.045))
            .px_3()
            .py(rems(0.625))
            .child(
                div().mt(px(2.)).child(
                    Icon::new(IconName::MessageCircle)
                        .size(rems(0.875))
                        .color(with_alpha(theme.primary, 0.7).into()),
                ),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .text_size(rems(0.875))
                    .line_height(rems(1.25))
                    .child(comment.text.clone()),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .opacity(0.)
                    .group_hover(group, |style| style.opacity(1.))
                    .child(
                        Button::new(ElementId::Name(
                            format!("diff-comment-delete-{}", comment.id).into(),
                        ))
                        .variant(ButtonVariant::GhostMuted)
                        .size(ButtonSize::IconXs)
                        .tooltip("Delete comment")
                        .child(Icon::new(IconName::Trash).size(rems(0.75)))
                        .on_click(move |_, _, cx| {
                            let id = id.clone();
                            panel.update(cx, |panel, cx| panel.delete_comment(&id, cx));
                        }),
                    ),
            )
            .into_any_element()
    }

    /// `DiffCommentAnnotation` kind `draft`.
    fn draft_comment(
        &mut self,
        range_label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use gpui_kit::base::input::Textarea;
        let theme = cx.app_theme().clone();
        if self.comment_input.is_none() {
            self.comment_input = Some(CommentInput::new(window, cx));
        }
        let state = self
            .comment_input
            .as_ref()
            .map(|input| input.state.clone())
            .unwrap_or_else(|| unreachable!());
        let text = state.read(cx).value().to_string();
        let can_send = !text.trim().is_empty();
        let panel = cx.entity();
        let cancel_panel = panel.clone();
        let send_panel = panel.clone();
        let send_state = state.clone();
        div()
            .flex()
            .flex_col()
            .px_3()
            .py_2()
            .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.cancel_draft(cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(theme.input)
                    .bg(theme.background)
                    .px_2()
                    .py_1()
                    .text_size(rems(0.875))
                    .child(Textarea::new(&state)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .mt(rems(0.375))
                    .child(
                        div()
                            .mr_auto()
                            .text_size(TEXT_3XS.size)
                            .text_color(with_alpha(theme.muted_foreground, 0.7))
                            .child(format!("⌘/Ctrl Enter to send · {range_label}")),
                    )
                    .child(
                        Button::new("diff-comment-cancel")
                            .variant(ButtonVariant::GhostMuted)
                            .size(ButtonSize::Xs)
                            .label("Cancel")
                            .on_click(move |_, _, cx| {
                                cancel_panel.update(cx, |panel, cx| panel.cancel_draft(cx));
                            }),
                    )
                    .child(
                        Button::new("diff-comment-send")
                            .size(ButtonSize::Xs)
                            .label("Comment")
                            .disabled(!can_send)
                            .on_click(move |_, _, cx| {
                                let text = send_state.read(cx).value().to_string();
                                send_panel
                                    .update(cx, |panel, cx| panel.try_submit_draft(&text, cx));
                            }),
                    ),
            )
            .into_any_element()
    }
}

struct RowCtx {
    colors: DiffColors,
    metrics: Metrics,
    digits: usize,
    wrap: bool,
    entry: FileEntry,
    file_index: usize,
}

fn px_rems(pixels: f32) -> gpui_kit::Rems {
    rems(pixels / 16.)
}

/// The number column: `[data-column-number]` padding-left 2ch, `digits` ch of
/// numbers, padding-right 1ch, then the 2px border-right of `@pierre/diffs`.
fn gutter_width_for(ch: gpui_kit::Pixels, digits: usize) -> gpui_kit::Pixels {
    ch * (2. + digits as f32 + 1.) + px(2.)
}

/// The highlighted line text.
fn code_text(
    text: &str,
    runs: &[(Range<usize>, Hsla)],
    _foreground: Hsla,
    _metrics: &Metrics,
    _wrap: bool,
) -> StyledText {
    let highlights: Vec<(Range<usize>, HighlightStyle)> = runs
        .iter()
        .filter(|(range, _)| range.start < range.end && range.end <= text.len())
        .map(|(range, color)| {
            (
                range.clone(),
                HighlightStyle {
                    color: Some(*color),
                    ..Default::default()
                },
            )
        })
        .collect();
    StyledText::new(text.to_string()).with_highlights(highlights)
}

impl Render for DiffPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        self.dark = theme.is_dark();
        if let Some((run_id, file_path)) = super::take_turn_request(cx) {
            self.open_turn_diff(&run_id, file_path.as_deref(), cx);
        }
        let toolbar = self.render_toolbar(window, cx);
        let body = self.render_body(window, cx);
        div()
            .id("diff-panel")
            .key_context("DiffPanel")
            .track_focus(&self.focus_handle)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.focus_handle, cx);
                }),
            )
            .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| {
                let key = event.keystroke.key.as_str();
                if key == "escape" {
                    if this.menu.take().is_some() {
                        cx.notify();
                    } else if this.draft.is_some() {
                        this.cancel_draft(cx);
                    } else if this.selection.take().is_some() {
                        cx.notify();
                    }
                    return;
                }
                if !event.keystroke.modifiers.modified() && this.scroll_key(key, cx) {
                    cx.stop_propagation();
                }
            }))
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .min_w_0()
            .bg(theme.background)
            .text_color(theme.foreground)
            .font_family(theme.fonts.sans.clone())
            .child(toolbar)
            .child(body)
    }
}

// Re-exported for the module map.
#[allow(non_snake_case)]
fn Derived_turn_count(turn: &crate::components::chat::timeline::types::TurnDiffSummary) -> u64 {
    turn.checkpoint_turn_count
}

#[allow(dead_code)]
const _AUTOMATIC: &str = AUTOMATIC_BASE_REF;

#[allow(dead_code)]
fn _range_unused(_: SelectedLineRange, _: LineLoadingUnused, _: LineTokens) {}
type LineLoadingUnused = ();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gutter_is_2ch_digits_1ch_and_a_2px_border() {
        assert_eq!(gutter_width_for(px(10.), 2), px(52.));
        assert_eq!(gutter_width_for(px(10.), 3), px(62.));
    }
}
