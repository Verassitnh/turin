//! Port of ModelPickerContent.tsx / ModelPickerSidebar.tsx / ModelListRow.tsx:
//! the provider/model popover. The list logic lives in
//! `composer::model_picker`; this view paints it and routes keys.
//!
//! Port of upstream/apps/web/src/components/chat/ModelPickerSidebar.tsx.
//! Port of upstream/apps/web/src/components/chat/ModelListRow.tsx.

use std::collections::HashSet;
use std::sync::Arc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, CursorStyle, Entity, EventEmitter, FocusHandle,
    Focusable, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
    rems,
};

use crate::composer::catalog::{ProviderInstanceEntry, get_provider_status_message};
use crate::composer::model_display::{get_display_model_name, get_provider_row_label};
use crate::composer::model_picker::{
    ModelPickerInput, ModelPickerItem, PickerRow, PickerSelection, adjacent_model_picker_provider,
    compute_model_picker_list, initial_expanded_legacy_instances, initial_picker_selection,
    provider_model_key,
};
use crate::composer::model_picker_keys::{
    parse_model_picker_legacy_section_key, parse_model_picker_model_key,
};
use crate::composer::view_model::{ComposerStore, ComposerViewModel};
use crate::composer::views::chrome::{menu_row, muted_label};
use crate::composer::views::footer::provider_mark;
use crate::composer::views::prompt_editor::{
    CommandKey, EditorConfig, PromptEditor, PromptEditorEvent,
};
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_MD, TEXT_XS};
use crate::ui::{Icon, IconName, Tooltip};

#[derive(Debug, Clone, PartialEq)]
pub enum ModelPickerEvent {
    Select { instance_id: String, slug: String },
    ToggleFavorite { instance_id: String, slug: String },
    OpenSetup(String),
    Close,
}

impl EventEmitter<ModelPickerEvent> for ModelPickerView {}

pub struct ModelPickerView {
    store: Entity<ComposerStore>,
    search: Entity<PromptEditor>,
    query: String,
    selected: PickerSelection,
    expanded_legacy: HashSet<String>,
    highlighted: Option<String>,
    scroll: ScrollHandle,
    /// Row keys in list order, for keyboard navigation.
    row_keys: Vec<String>,
    /// `modelKey` → `(instance, slug)` for the first nine selectable rows.
    jump_keys: Vec<String>,
    height_lock: Option<f32>,
}

impl ModelPickerView {
    pub fn new(store: Entity<ComposerStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let vm = store.read(cx).view_model().clone();
        let favorites = vm.favorite_models.clone();
        let input = ModelPickerInput {
            instance_entries: &vm.providers,
            active_instance_id: &vm.selected_instance_id,
            model: &vm.selected_model,
            locked_provider: vm.locked_provider.as_deref(),
            locked_continuation_group_key: vm.locked_continuation_group_key.as_deref(),
            favorites: &favorites,
            search_query: "",
            selected: &PickerSelection::Favorites,
            expanded_legacy_instances: &HashSet::new(),
            on_open_provider_setup: true,
        };
        let selected = initial_picker_selection(&input);
        let expanded_legacy = initial_expanded_legacy_instances(&input);
        let search = cx.new(|cx| {
            PromptEditor::new(
                "",
                EditorConfig {
                    placeholder: "Search models...".into(),
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
            editor.set_command_handler(Some(std::rc::Rc::new(move |state, key, window, cx| {
                let empty = state.is_empty();
                weak.update(cx, |this, cx| this.on_search_key(key, empty, window, cx))
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
                    this.highlighted = None;
                    cx.notify();
                }
            },
        )
        .detach();
        cx.observe(&store, |_, _, cx| cx.notify()).detach();
        Self {
            store,
            search,
            query: String::new(),
            selected,
            expanded_legacy,
            highlighted: None,
            scroll: ScrollHandle::new(),
            row_keys: Vec::new(),
            jump_keys: Vec::new(),
            height_lock: None,
        }
    }

    pub fn focus_search(&self, window: &mut Window, cx: &mut Context<Self>) {
        let search = self.search.clone();
        search.update(cx, |editor, cx| editor.focus_at_end(window, cx));
    }

    fn on_search_key(
        &mut self,
        key: &CommandKey,
        search_empty: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let plain = !key.alt && !key.ctrl && !key.meta;
        match key.key.as_str() {
            "Escape" => {
                cx.emit(ModelPickerEvent::Close);
                true
            }
            "Enter" if plain => {
                if let Some(highlighted) = self
                    .highlighted
                    .clone()
                    .or_else(|| self.row_keys.first().cloned())
                {
                    self.activate(&highlighted, cx);
                }
                true
            }
            "ArrowDown" | "ArrowUp" if plain => {
                self.move_highlight(if key.key == "ArrowDown" { 1 } else { -1 }, cx);
                true
            }
            "ArrowLeft" if plain && !key.shift && search_empty => {
                self.step_provider(-1, cx);
                true
            }
            "ArrowRight" if plain && !key.shift && search_empty => {
                self.step_provider(1, cx);
                true
            }
            "Tab" if plain => {
                self.step_provider(if key.shift { -1 } else { 1 }, cx);
                true
            }
            digit
                if key.meta
                    && digit.len() == 1
                    && digit.chars().all(|c| ('1'..='9').contains(&c)) =>
            {
                let index = digit.parse::<usize>().unwrap_or(1) - 1;
                if let Some(target) = self.jump_keys.get(index).cloned() {
                    self.activate(&target, cx);
                }
                true
            }
            _ => false,
        }
    }

    fn step_provider(&mut self, direction: i32, cx: &mut Context<Self>) {
        let vm = self.store.read(cx).view_model().clone();
        let list = self.list(&vm);
        let next = adjacent_model_picker_provider(
            &vm.providers,
            &self.selected,
            direction,
            list.locked_disabled_instance_ids.as_ref(),
            list.selectable_unavailable_instance_ids.as_ref(),
        );
        self.select_provider(next, cx);
    }

    fn select_provider(&mut self, selection: PickerSelection, cx: &mut Context<Self>) {
        self.selected = selection;
        self.highlighted = None;
        cx.notify();
    }

    fn move_highlight(&mut self, delta: i32, cx: &mut Context<Self>) {
        if self.row_keys.is_empty() {
            return;
        }
        let current = self
            .highlighted
            .as_ref()
            .and_then(|key| self.row_keys.iter().position(|row| row == key));
        let next = match current {
            None => {
                if delta > 0 {
                    0
                } else {
                    self.row_keys.len() - 1
                }
            }
            Some(index) => (index as i32 + delta).rem_euclid(self.row_keys.len() as i32) as usize,
        };
        self.highlighted = Some(self.row_keys[next].clone());
        self.scroll.scroll_to_item(next);
        cx.notify();
    }

    fn activate(&mut self, key: &str, cx: &mut Context<Self>) {
        if let Some(instance_id) = parse_model_picker_legacy_section_key(key) {
            if !self.expanded_legacy.remove(&instance_id) {
                self.expanded_legacy.insert(instance_id);
            }
            cx.notify();
            return;
        }
        if let Some((instance_id, slug)) = parse_model_picker_model_key(key) {
            cx.emit(ModelPickerEvent::Select { instance_id, slug });
        }
    }

    fn list(&self, vm: &ComposerViewModel) -> crate::composer::model_picker::ModelPickerList {
        compute_model_picker_list(&ModelPickerInput {
            instance_entries: &vm.providers,
            active_instance_id: &vm.selected_instance_id,
            model: &vm.selected_model,
            locked_provider: vm.locked_provider.as_deref(),
            locked_continuation_group_key: vm.locked_continuation_group_key.as_deref(),
            favorites: &vm.favorite_models,
            search_query: &self.query,
            selected: &self.selected,
            expanded_legacy_instances: &self.expanded_legacy,
            on_open_provider_setup: true,
        })
    }

    fn sidebar(
        &mut self,
        vm: &Arc<ComposerViewModel>,
        list: &crate::composer::model_picker::ModelPickerList,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let selected = self.selected.clone();
        let button = |id: SharedString,
                      selected: bool,
                      disabled: bool,
                      content: AnyElement,
                      tooltip: String| {
            div()
                .id(id)
                .relative()
                .flex()
                .w_full()
                .h(rems(2.25))
                .items_center()
                .justify_center()
                .rounded(RADIUS_MD)
                .when(selected, |this| this.bg(with_alpha(theme.foreground, 0.08)))
                .when(!disabled, |this| {
                    this.cursor(CursorStyle::PointingHand)
                        .hover(|style| style.bg(with_alpha(theme.foreground, 0.10)))
                })
                .when(disabled, |this| {
                    this.opacity(0.5).cursor(CursorStyle::OperationNotAllowed)
                })
                .child(content)
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        };
        let mut column = div()
            .relative()
            .flex()
            .flex_col()
            .gap_1()
            .p_1()
            .min_h_full();
        column = column.child(
            button(
                "picker-favorites".into(),
                selected == PickerSelection::Favorites,
                false,
                Icon::new(IconName::Star)
                    .size(rems(1.25))
                    .color(theme.foreground)
                    .into_any_element(),
                "Favorites".to_owned(),
            )
            .on_click(
                cx.listener(|this, _, _, cx| this.select_provider(PickerSelection::Favorites, cx)),
            ),
        );
        column = column.child(
            div()
                .border_b_1()
                .border_color(with_alpha(theme.border, 0.7)),
        );
        for entry in &list.sidebar_entries {
            let unavailable = !entry.picker_ready();
            let reachable = list
                .selectable_unavailable_instance_ids
                .as_ref()
                .is_some_and(|ids| ids.contains(&entry.instance_id));
            let context_disabled = list
                .locked_disabled_instance_ids
                .as_ref()
                .is_some_and(|ids| ids.contains(&entry.instance_id));
            let disabled = (unavailable && !reachable) || context_disabled;
            let tooltip = if unavailable {
                get_provider_status_message(entry)
            } else if context_disabled {
                format!(
                    "{} is unavailable in this thread. Start a new thread to switch providers.",
                    entry.display_name
                )
            } else {
                entry.display_name.clone()
            };
            let instance_id = entry.instance_id.clone();
            let is_selected = selected == PickerSelection::Instance(entry.instance_id.clone());
            column = column.child(
                button(
                    format!("picker-instance-{}", entry.instance_id).into(),
                    is_selected,
                    disabled,
                    provider_mark(&entry.driver_kind, rems(1.25), theme.foreground)
                        .into_any_element(),
                    tooltip,
                )
                .when(!disabled, |this| {
                    this.on_click(cx.listener(move |this, _, _, cx| {
                        this.select_provider(PickerSelection::Instance(instance_id.clone()), cx)
                    }))
                }),
            );
        }
        let _ = vm;
        div()
            .w(rems(2.75))
            .flex_shrink_0()
            .overflow_hidden()
            .bg(with_alpha(theme.muted, 0.3))
            .child(column)
            .into_any_element()
    }

    fn model_row(
        &mut self,
        item: &ModelPickerItem,
        index: usize,
        vm: &Arc<ComposerViewModel>,
        jump_label: Option<String>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let key = item.key();
        let is_active =
            vm.selected_instance_id == item.instance_id && vm.selected_model == item.model.slug;
        let highlighted = self.highlighted.as_deref() == Some(key.as_str());
        let favorite = vm
            .favorite_models
            .contains(&provider_model_key(&item.instance_id, &item.model.slug));
        let prefer_short = vm.locked_provider.is_none();
        let name = get_display_model_name(&item.model, prefer_short);
        let provider_label = get_provider_row_label(
            &item.instance_display_name,
            item.model.sub_provider.as_deref(),
        );
        let (instance, slug) = (item.instance_id.clone(), item.model.slug.clone());
        let (fav_instance, fav_slug) = (item.instance_id.clone(), item.model.slug.clone());
        let activate_key = key.clone();
        menu_row(
            ("picker-model", index),
            highlighted || is_active && !highlighted && false,
            cx,
        )
        .when(is_active && !highlighted, |this| {
            this.bg(with_alpha(theme.foreground, 0.08))
        })
        .w_full()
        .child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .flex_1()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(TEXT_XS.size)
                                .font_weight(FontWeight::MEDIUM)
                                .child(name),
                        )
                        .when(item.model.is_new, |this| {
                            this.child(
                                div()
                                    .flex_shrink_0()
                                    .rounded(px(4.))
                                    .border_1()
                                    .border_color(with_alpha(theme.update, 0.35))
                                    .bg(with_alpha(theme.update, 0.15))
                                    .px(px(2.))
                                    .text_color(theme.update_foreground)
                                    .text_size(rems(0.625))
                                    .font_weight(FontWeight::BOLD)
                                    .child("NEW"),
                            )
                        })
                        .when(item.model.is_unavailable, |this| {
                            this.child(
                                div()
                                    .flex_shrink_0()
                                    .rounded(px(6.))
                                    .border_1()
                                    .border_color(theme.border)
                                    .px(px(5.))
                                    .text_size(rems(0.6875))
                                    .child("Unavailable"),
                            )
                        }),
                )
                .child(
                    div()
                        .mt_1()
                        .flex()
                        .items_center()
                        .gap(rems(0.375))
                        .child(provider_mark(
                            &item.driver_kind,
                            rems(0.75),
                            theme.muted_foreground,
                        ))
                        .child(
                            div()
                                .truncate()
                                .text_size(TEXT_XS.size)
                                .text_color(with_alpha(theme.muted_foreground, 0.7))
                                .child(provider_label),
                        ),
                ),
        )
        .child(
            div()
                .flex()
                .flex_shrink_0()
                .items_center()
                .gap(rems(0.375))
                .when_some(jump_label, |this, label| {
                    this.child(
                        div()
                            .text_size(TEXT_XS.size)
                            .text_color(theme.muted_foreground)
                            .child(label),
                    )
                })
                .child(
                    div()
                        .id(("picker-favorite", index))
                        .flex()
                        .size(rems(1.25))
                        .items_center()
                        .justify_center()
                        .rounded(px(6.))
                        .cursor(CursorStyle::PointingHand)
                        .hover(|style| style.bg(with_alpha(theme.foreground, 0.1)))
                        .child(
                            Icon::new(IconName::Star)
                                .size(rems(0.75))
                                .color(if favorite {
                                    theme.warning
                                } else {
                                    theme.muted_foreground
                                }),
                        )
                        .tooltip(move |window, cx| {
                            Tooltip::new(if favorite {
                                "Remove from favorites"
                            } else {
                                "Add to favorites"
                            })
                            .build(window, cx)
                        })
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.stop_propagation();
                            cx.emit(ModelPickerEvent::ToggleFavorite {
                                instance_id: fav_instance.clone(),
                                slug: fav_slug.clone(),
                            });
                        })),
                ),
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            let _ = (&instance, &slug);
            this.activate(&activate_key, cx);
        }))
        .into_any_element()
    }

    fn legacy_row(
        &mut self,
        key: &str,
        count: usize,
        expanded: bool,
        index: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let highlighted = self.highlighted.as_deref() == Some(key);
        let activate_key = key.to_owned();
        menu_row(("picker-legacy", index), highlighted, cx)
            .w_full()
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .child(
                        div()
                            .text_size(TEXT_XS.size)
                            .font_weight(FontWeight::MEDIUM)
                            .child("Legacy models"),
                    )
                    .child(
                        div()
                            .mt_1()
                            .text_size(TEXT_XS.size)
                            .text_color(with_alpha(theme.muted_foreground, 0.7))
                            .child(format!("{count} models")),
                    ),
            )
            .child(
                Icon::new(IconName::ChevronRight)
                    .color(theme.muted_foreground)
                    .when(expanded, |icon| icon.size(rems(1.))),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.activate(&activate_key, cx)))
            .into_any_element()
    }
}

impl Focusable for ModelPickerView {
    fn focus_handle(&self, cx: &gpui_kit::App) -> FocusHandle {
        self.search.read(cx).focus_handle(cx)
    }
}

impl Render for ModelPickerView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let vm = self.store.read(cx).view_model().clone();
        let list = self.list(&vm);
        self.row_keys = list.rows.iter().map(PickerRow::key).collect();
        self.jump_keys = list.jump_keys.clone();
        let is_searching = list.is_searching;
        if !is_searching {
            self.height_lock = None;
        }

        let mut rows: Vec<AnyElement> = Vec::new();
        for (index, row) in list.rows.iter().enumerate() {
            match row {
                PickerRow::Model(item) => {
                    let jump_label = list
                        .jump_keys
                        .iter()
                        .position(|key| *key == item.key())
                        .and_then(|position| {
                            vm.shortcut_labels
                                .get(&format!("modelPicker.jump.{}", position + 1))
                                .cloned()
                        });
                    rows.push(self.model_row(item, index, &vm, jump_label, cx));
                }
                PickerRow::LegacySection {
                    key,
                    count,
                    expanded,
                } => rows.push(self.legacy_row(key, *count, *expanded, index, cx)),
            }
            rows.push(div().h(px(2.)).into_any_element());
        }
        let empty = list.rows.is_empty() && list.provider_setup_entries.is_empty();

        let setup_entries: Vec<ProviderInstanceEntry> = list.provider_setup_entries.clone();
        let multiple_setup = setup_entries.len() > 1;
        let sidebar = list.show_sidebar.then(|| self.sidebar(&vm, &list, cx));

        let main = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .bg(with_alpha(theme.muted, 0.4))
            .when(list.show_sidebar, |this| {
                this.border_l_1()
                    .border_color(with_alpha(theme.border, 0.7))
            })
            .child(
                // ComboboxSearchInput: `px-3 pt-2.5`, bordered underline, search glyph.
                div().flex_shrink_0().px_3().pt(rems(0.625)).child(
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
                    .id("picker-list")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .py(rems(0.375))
                    .pl_2()
                    .pr(px(1.))
                    .children(rows)
                    .when(empty, |this| {
                        this.child(muted_label("No models found", cx).px_3().py_2())
                    }),
            )
            .when(!setup_entries.is_empty(), |this| {
                this.child(
                    div()
                        .flex_shrink_0()
                        .max_h(rems(11.))
                        .overflow_hidden()
                        .border_t_1()
                        .border_color(with_alpha(theme.border, 0.7))
                        .p_2()
                        .children(setup_entries.iter().map(|entry| {
                            let instance_id = entry.instance_id.clone();
                            let label = if multiple_setup {
                                format!("Set up {}", entry.display_name)
                            } else {
                                "Open provider setup".to_owned()
                            };
                            div()
                                .px_1()
                                .py(rems(0.375))
                                .text_size(TEXT_XS.size)
                                .child(
                                    div()
                                        .text_color(theme.muted_foreground)
                                        .child(get_provider_status_message(entry)),
                                )
                                .child(
                                    div()
                                        .id(("picker-setup", entry.instance_id.len()))
                                        .mt_1()
                                        .cursor(CursorStyle::PointingHand)
                                        .text_color(theme.foreground)
                                        .underline()
                                        .child(label)
                                        .on_click(cx.listener(move |_, _, _, cx| {
                                            cx.emit(ModelPickerEvent::OpenSetup(
                                                instance_id.clone(),
                                            ))
                                        })),
                                )
                        })),
                )
            })
            .children(list.update_required_notices.iter().map(|(_, notice)| {
                div()
                    .flex_shrink_0()
                    .border_t_1()
                    .border_color(with_alpha(theme.border, 0.7))
                    .px_3()
                    .py_2()
                    .text_size(TEXT_XS.size)
                    .text_color(theme.muted_foreground)
                    .child(notice.clone())
            }));

        // The list holds its natural height up to the picker's cap.
        let estimated =
            3.25 + list.rows.len() as f32 * 2.85 + if setup_entries.is_empty() { 0.5 } else { 7. };
        div()
            .flex()
            .flex_row()
            .w(rems(22.5))
            .h(rems(
                estimated
                    .max(2. + (list.sidebar_entries.len() as f32 + 1.) * 2.5)
                    .clamp(7., 21.625),
            ))
            .overflow_hidden()
            .when_some(sidebar, |this, sidebar| this.child(sidebar))
            .child(main)
    }
}
