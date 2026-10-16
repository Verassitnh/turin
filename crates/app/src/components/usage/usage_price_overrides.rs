//! Port of upstream/apps/web/src/components/usage/UsagePriceOverrides.tsx
//!
//! Edits custom model prices and mappings. A mapped model's usage counts as
//! its target model. `initial_model` opens with a new row for that model.
//!
//! Not ported: the `<datalist>` model suggestions on the model and "Map to"
//! fields (GPUI inputs have no native autocomplete list).

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Window, deferred, div, px, relative, rems,
};
use indexmap::IndexMap;
use turin_contracts::EnvironmentId;

use super::backend::UsageBackend;
use super::usage_price_form::{USAGE_PRICE_FIELDS, UsagePriceField};
use super::usage_price_table::{
    UsagePriceCell, UsagePriceDraft, is_empty_usage_price_draft, usage_alias_cell,
    usage_price_cell, usage_price_table_changes, usage_price_table_errors,
};
use super::usage_price_targets::{
    UsagePriceChange, UsagePriceTarget, UsagePriceWriteResult, write_usage_prices,
};
use super::usage_ui::{compact_input, menu_checkbox_item, menu_separator};
use super::view_model::{EnvironmentSelection, EnvironmentUsageStatus};
use crate::diff::menu::menu_surface;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_LG, TEXT_XS};
use crate::ui::dialog::{dialog_footer, dialog_header, dialog_panel, dialog_popup};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName, Tooltip};

/// Per-destination state of the last save, shown under the table.
#[derive(Clone, Debug, PartialEq, Eq)]
enum AttemptResult {
    Saving,
    Done(UsagePriceWriteResult),
}

#[derive(Clone, Debug)]
struct SaveAttempt {
    drafts: Vec<UsagePriceDraft>,
    destinations: Vec<(EnvironmentId, String)>,
    results: IndexMap<EnvironmentId, AttemptResult>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Column {
    Model,
    Alias,
    Price(UsagePriceField),
}

impl Column {
    fn key(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Alias => "alias",
            Self::Price(field) => field.spec().key,
        }
    }
}

pub struct UsagePriceOverrides {
    backend: Rc<dyn UsageBackend>,
    #[allow(dead_code)]
    usage: Vec<EnvironmentUsageStatus>,
    selected_ids: EnvironmentSelection,
    drafts: Vec<UsagePriceDraft>,
    pending: bool,
    attempt: Option<SaveAttempt>,
    next_row_id: u32,
    menu_open: bool,
    inputs: HashMap<(String, Column), Entity<InputState>>,
    placeholders: HashMap<(String, Column), String>,
    subscriptions: Vec<Subscription>,
}

fn selected_targets(
    environments: &[UsagePriceTarget],
    selected_ids: &EnvironmentSelection,
) -> Vec<UsagePriceTarget> {
    environments
        .iter()
        .filter(|environment| {
            selected_ids
                .as_ref()
                .is_none_or(|ids| ids.contains(&environment.environment_id))
        })
        .cloned()
        .collect()
}

fn js_trim(value: &str) -> &str {
    turin_contracts::base_schemas::js_trim(value)
}

impl UsagePriceOverrides {
    pub fn new(
        backend: Rc<dyn UsageBackend>,
        usage: Vec<EnvironmentUsageStatus>,
        initial_selected_environment_ids: EnvironmentSelection,
        initial_model: Option<String>,
    ) -> Self {
        let environments = backend.price_targets();
        let selected = selected_targets(&environments, &initial_selected_environment_ids);
        // A model that already has a custom price or mapping is edited in its existing row.
        let drafts = match initial_model {
            Some(model)
                if !selected.iter().any(|environment| {
                    environment
                        .prices
                        .as_ref()
                        .is_some_and(|prices| prices.contains_key(&model))
                        || environment
                            .aliases
                            .as_ref()
                            .is_some_and(|aliases| aliases.contains_key(&model))
                }) =>
            {
                vec![UsagePriceDraft::new("new:initial", model, true)]
            }
            _ => Vec::new(),
        };
        Self {
            backend,
            usage,
            selected_ids: initial_selected_environment_ids,
            drafts,
            pending: false,
            attempt: None,
            next_row_id: 0,
            menu_open: false,
            inputs: HashMap::new(),
            placeholders: HashMap::new(),
            subscriptions: Vec::new(),
        }
    }

    fn reset_inputs(&mut self) {
        self.inputs.clear();
        self.placeholders.clear();
        self.subscriptions.clear();
    }

    fn update_draft(&mut self, draft: UsagePriceDraft) {
        match self.drafts.iter_mut().find(|entry| entry.id == draft.id) {
            Some(entry) => *entry = draft,
            None => self.drafts.push(draft),
        }
        self.attempt = None;
    }

    fn update_or_drop_draft(&mut self, draft: UsagePriceDraft) {
        if !draft.is_new && draft.values.is_empty() && draft.alias.is_none() && !draft.removed {
            self.drafts.retain(|entry| entry.id != draft.id);
        } else {
            self.update_draft(draft);
        }
    }

    /// An existing row edited back to its saved values has nothing left to save.
    fn matches_saved(row: &UsagePriceDraft, cell: &UsagePriceCell, value: &str) -> bool {
        !row.is_new
            && cell.placeholder != "Mixed"
            && cell.placeholder != "Unavailable"
            && value == cell.value
    }

    fn row_draft(&self, id: &str, model: &str) -> UsagePriceDraft {
        self.drafts
            .iter()
            .find(|draft| draft.id == id)
            .cloned()
            .unwrap_or_else(|| UsagePriceDraft::new(id, model, false))
    }

    fn on_input_change(
        &mut self,
        row_id: &str,
        column: Column,
        value: String,
        cx: &mut Context<Self>,
    ) {
        let environments = self.backend.price_targets();
        let selected = selected_targets(&environments, &self.selected_ids);
        let Some(mut row) = self
            .drafts
            .iter()
            .find(|d| d.id == row_id)
            .cloned()
            .or_else(|| {
                row_id
                    .strip_prefix("model:")
                    .map(|model| UsagePriceDraft::new(row_id, model, false))
            })
        else {
            return;
        };
        match column {
            Column::Model => {
                row.model = value;
                self.update_draft(row);
            }
            Column::Alias => {
                let original = usage_alias_cell(&selected, &row.model);
                row.alias = None;
                if !Self::matches_saved(&row, &original, &value) {
                    row.alias = Some(value);
                }
                self.update_or_drop_draft(row);
            }
            Column::Price(field) => {
                let original = usage_price_cell(&selected, &row.model, field);
                if Self::matches_saved(&row, &original, &value) {
                    row.values.remove(&field);
                } else {
                    row.values.insert(field, value);
                }
                self.update_or_drop_draft(row);
            }
        }
        cx.notify();
    }

    fn input(
        &mut self,
        row_id: &str,
        column: Column,
        initial: &str,
        placeholder: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        let key = (row_id.to_owned(), column);
        if let Some(state) = self.inputs.get(&key) {
            let state = state.clone();
            if self.placeholders.get(&key).map(String::as_str) != Some(placeholder) {
                self.placeholders.insert(key, placeholder.to_owned());
                let placeholder = placeholder.to_owned();
                state.update(cx, |state, cx| {
                    state.set_placeholder(placeholder, window, cx)
                });
            }
            return state;
        }
        self.placeholders
            .insert(key.clone(), placeholder.to_owned());
        let state = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(placeholder.to_owned())
                .default_value(initial.to_owned())
        });
        let row = row_id.to_owned();
        self.subscriptions.push(cx.subscribe_in(
            &state,
            window,
            move |this, input, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = input.read(cx).value().to_string();
                    this.on_input_change(&row, column, value, cx);
                }
            },
        ));
        self.inputs.insert(key, state.clone());
        state
    }

    fn select_environments(&mut self, ids: EnvironmentSelection) {
        self.selected_ids = ids;
        self.attempt = None;
        self.reset_inputs();
    }

    fn discard(&mut self) {
        self.drafts.clear();
        self.attempt = None;
        self.reset_inputs();
    }

    fn save(&mut self, retry: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending {
            return;
        }
        let environments = self.backend.price_targets();
        let selected = selected_targets(&environments, &self.selected_ids);
        let staged: Vec<UsagePriceDraft> = self
            .drafts
            .iter()
            .filter(|draft| !is_empty_usage_price_draft(draft))
            .cloned()
            .collect();
        let failed_destinations: Vec<(EnvironmentId, String)> = self
            .attempt
            .as_ref()
            .map(|attempt| {
                attempt
                    .destinations
                    .iter()
                    .filter(|(id, _)| {
                        matches!(
                            attempt.results.get(id),
                            Some(AttemptResult::Done(UsagePriceWriteResult::Failed { .. }))
                        )
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let destinations: Vec<(EnvironmentId, String)> = if retry {
            failed_destinations
        } else {
            selected
                .iter()
                .map(|target| (target.environment_id.clone(), target.label.clone()))
                .collect()
        };
        let edits: Vec<UsagePriceDraft> = match (&self.attempt, retry) {
            (Some(attempt), true) => attempt.drafts.clone(),
            _ => staged,
        };
        if destinations.is_empty() || edits.is_empty() {
            return;
        }
        let targets: Vec<UsagePriceTarget> = destinations
            .iter()
            .map(|(id, label)| {
                environments
                    .iter()
                    .find(|environment| &environment.environment_id == id)
                    .cloned()
                    .unwrap_or_else(|| UsagePriceTarget {
                        environment_id: id.clone(),
                        label: label.clone(),
                        prices: None,
                        aliases: None,
                        unavailable: Some("Environment removed".to_owned()),
                    })
            })
            .collect();
        let plans: HashMap<EnvironmentId, super::usage_price_table::UsagePriceTableChanges> =
            targets
                .iter()
                .map(|target| {
                    (
                        target.environment_id.clone(),
                        usage_price_table_changes(target, &edits),
                    )
                })
                .collect();
        self.pending = true;
        let previous = if retry { self.attempt.take() } else { None };
        let mut results = previous
            .as_ref()
            .map(|attempt| attempt.results.clone())
            .unwrap_or_default();
        for target in &targets {
            results.insert(target.environment_id.clone(), AttemptResult::Saving);
        }
        self.attempt = Some(SaveAttempt {
            drafts: edits,
            destinations: previous
                .map(|attempt| attempt.destinations)
                .unwrap_or_else(|| {
                    targets
                        .iter()
                        .map(|target| (target.environment_id.clone(), target.label.clone()))
                        .collect()
                }),
            results,
        });
        cx.notify();

        let write_targets: Vec<UsagePriceTarget> = targets
            .iter()
            .map(|target| UsagePriceTarget {
                unavailable: target.unavailable.clone().or_else(|| {
                    plans
                        .get(&target.environment_id)
                        .and_then(|plan| plan.errors.values().next().cloned())
                }),
                ..target.clone()
            })
            .collect();
        let changes: HashMap<EnvironmentId, Vec<UsagePriceChange>> = plans
            .into_iter()
            .map(|(id, plan)| (id, plan.changes))
            .collect();
        let backend = self.backend.clone();
        let weak = cx.weak_entity();
        let app = cx.to_async();
        let failed = Rc::new(std::cell::Cell::new(false));
        cx.spawn_in(window, async move |this, cx| {
            let write_app = app.clone();
            let result_app = app.clone();
            let failed_flag = failed.clone();
            write_usage_prices(
                &write_targets,
                &changes,
                move |environment_id, patch| {
                    let backend = backend.clone();
                    let app = write_app.clone();
                    async move {
                        let task =
                            app.update(|app| backend.write_price_patch(environment_id, patch, app));
                        task.await
                    }
                },
                move |environment_id, result| {
                    if matches!(result, UsagePriceWriteResult::Failed { .. }) {
                        failed_flag.set(true);
                    }
                    let mut app = result_app.clone();
                    let id = environment_id.clone();
                    weak.update(&mut app, move |this, cx| {
                        if let Some(attempt) = this.attempt.as_mut() {
                            attempt.results.insert(id, AttemptResult::Done(result));
                        }
                        cx.notify();
                    })
                    .ok();
                },
            )
            .await;
            this.update_in(cx, |this, _, cx| {
                this.pending = false;
                if !failed.get() {
                    this.drafts.clear();
                    this.reset_inputs();
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl Render for UsagePriceOverrides {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let entity = cx.entity();
        let environments = self.backend.price_targets();
        let selected = selected_targets(&environments, &self.selected_ids);
        let custom_models: Vec<String> = {
            let mut models: Vec<String> = selected
                .iter()
                .flat_map(|environment| {
                    environment
                        .prices
                        .iter()
                        .flat_map(|prices| prices.keys().cloned())
                        .chain(
                            environment
                                .aliases
                                .iter()
                                .flat_map(|aliases| aliases.keys().cloned()),
                        )
                })
                .collect();
            models.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            models.dedup();
            models
        };
        // Keep new rows in place while successful environments publish their updated settings.
        let new_models: HashSet<String> = self
            .drafts
            .iter()
            .filter(|draft| draft.is_new)
            .map(|draft| js_trim(&draft.model).to_owned())
            .collect();
        let rows: Vec<UsagePriceDraft> = custom_models
            .iter()
            .filter(|model| self.attempt.is_none() || !new_models.contains(*model))
            .map(|model| self.row_draft(&format!("model:{model}"), model))
            .chain(self.drafts.iter().filter(|draft| draft.is_new).cloned())
            .collect();
        let staged: Vec<UsagePriceDraft> = self
            .drafts
            .iter()
            .filter(|draft| !is_empty_usage_price_draft(draft))
            .cloned()
            .collect();
        let mut errors = usage_price_table_errors(&selected, &staged);
        for draft in &staged {
            if !draft.is_new {
                continue;
            }
            let model = js_trim(&draft.model);
            if model.is_empty() {
                errors.insert(draft.id.clone(), "Enter a model ID.".to_owned());
            } else if self.attempt.is_none()
                && (custom_models.iter().any(|custom| custom == model)
                    || self
                        .drafts
                        .iter()
                        .any(|other| other.id != draft.id && js_trim(&other.model) == model))
            {
                errors.insert(
                    draft.id.clone(),
                    "This model already has a row. Edit its prices there.".to_owned(),
                );
            }
        }
        let failed_destinations: Vec<(EnvironmentId, String)> = self
            .attempt
            .as_ref()
            .map(|attempt| {
                attempt
                    .destinations
                    .iter()
                    .filter(|(id, _)| {
                        matches!(
                            attempt.results.get(id),
                            Some(AttemptResult::Done(UsagePriceWriteResult::Failed { .. }))
                        )
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let locked = self.pending || !failed_destinations.is_empty();
        let has_changes = !staged.is_empty();
        let destination_label = if selected.len() == 1 {
            selected[0].label.clone()
        } else {
            format!("{} environments", selected.len())
        };
        let selection_label = if self.selected_ids.is_none() {
            "All environments".to_owned()
        } else {
            destination_label.clone()
        };

        // ── Header ──────────────────────────────────────────────────────────
        let header = dialog_header(
            "Custom model prices",
            Some("Prices and mappings apply to all past and future usage on the environments you select.".into()),
            true,
            &theme,
        );

        // ── Apply-to menu ───────────────────────────────────────────────────
        let menu_disabled = self.pending || has_changes;
        let apply_button = {
            let entity = entity.clone();
            Button::new("usage-prices-apply")
                .variant(ButtonVariant::Outline)
                .size(ButtonSize::Sm)
                .disabled(menu_disabled)
                .pressed(self.menu_open)
                .child(div().min_w_0().truncate().child(selection_label))
                .child(Icon::new(IconName::ChevronDown).size(rems(0.875)))
                .on_click(move |_, _, cx| {
                    entity.update(cx, |this, cx| {
                        this.menu_open = !this.menu_open;
                        cx.notify();
                    });
                })
        };
        let menu = self.menu_open.then(|| {
            let all_selected = self.selected_ids.is_none();
            let all_entity = entity.clone();
            let mut surface = menu_surface(cx)
                .absolute()
                .top(px(34.))
                .left_0()
                .min_w(rems(12.))
                .child(
                    menu_checkbox_item(
                        "usage-prices-all",
                        all_selected,
                        "All environments",
                        &theme,
                    )
                    .on_click(move |_, _, cx| {
                        all_entity.update(cx, |this, cx| {
                            let next = if this.selected_ids.is_none() {
                                Some(HashSet::new())
                            } else {
                                None
                            };
                            this.select_environments(next);
                            cx.notify();
                        });
                    }),
                )
                .child(menu_separator(&theme));
            for environment in &environments {
                let checked = self
                    .selected_ids
                    .as_ref()
                    .is_none_or(|ids| ids.contains(&environment.environment_id));
                let toggle_entity = entity.clone();
                let id = environment.environment_id.clone();
                let total = environments.len();
                let current: HashSet<EnvironmentId> = selected
                    .iter()
                    .map(|target| target.environment_id.clone())
                    .collect();
                surface = surface.child(
                    menu_checkbox_item(
                        SharedString::from(format!("usage-prices-env:{}", id.as_str())),
                        checked,
                        div()
                            .flex()
                            .min_w_0()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .truncate()
                                    .child(environment.label.clone()),
                            )
                            .children(environment.unavailable.clone().map(|text| {
                                div()
                                    .text_size(TEXT_XS.size)
                                    .text_color(theme.muted_foreground)
                                    .child(text)
                            })),
                        &theme,
                    )
                    .on_click(move |_, _, cx| {
                        let id = id.clone();
                        let current = current.clone();
                        toggle_entity.update(cx, |this, cx| {
                            let mut next = current;
                            if !checked {
                                next.insert(id);
                            } else {
                                next.remove(&id);
                            }
                            let value = if next.len() == total {
                                None
                            } else {
                                Some(next)
                            };
                            this.select_environments(value);
                            cx.notify();
                        });
                    }),
                );
            }
            let dismiss = entity.clone();
            deferred(surface.on_mouse_down_out(move |_, _, cx| {
                dismiss.update(cx, |this, cx| {
                    this.menu_open = false;
                    cx.notify();
                });
            }))
            .with_priority(40)
        });

        // ── Table ───────────────────────────────────────────────────────────
        let head_cell = |label: SharedString| {
            div()
                .h(rems(2.5))
                .flex()
                .flex_shrink_0()
                .items_center()
                .px_2()
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.foreground)
                .child(label)
        };
        let price_columns = USAGE_PRICE_FIELDS.len() as f32;
        let price_width = (0.6) / price_columns;
        let add_entity = entity.clone();
        let table_header = div()
            .flex()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .child(head_cell("Model ID".into()).w(relative(0.22)))
            .child(head_cell("Map to".into()).w(relative(0.18)))
            .children(USAGE_PRICE_FIELDS.iter().map(|field| {
                head_cell(field.label.into())
                    .w(relative(price_width))
                    .flex_shrink_0()
            }))
            .child(
                div()
                    .w(rems(2.5))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        Button::new("usage-price-add")
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::IconXs)
                            .icon(IconName::Plus)
                            .disabled(locked)
                            .on_click(move |_, _, cx| {
                                add_entity.update(cx, |this, cx| {
                                    let id = format!("new:{}", this.next_row_id);
                                    this.next_row_id += 1;
                                    this.update_draft(UsagePriceDraft::new(id, "", true));
                                    cx.notify();
                                });
                            }),
                    ),
            );

        let mut row_elements: Vec<AnyElement> = Vec::new();
        let last = rows.len().saturating_sub(1);
        for (index, row) in rows.iter().enumerate() {
            let alias_cell = usage_alias_cell(&selected, &row.model);
            let alias_value = row
                .alias
                .clone()
                .unwrap_or_else(|| alias_cell.value.clone());
            let alias = js_trim(&alias_value).to_owned();
            let error = errors.get(&row.id).filter(|_| {
                !js_trim(&row.model).is_empty()
                    || row.values.values().any(|value| !value.is_empty())
            });
            let cell = |width: f32| div().w(relative(width)).flex_shrink_0().min_w_0().p_2();

            let model_cell = if row.is_new {
                let state = self.input(&row.id, Column::Model, &row.model, "Model ID", window, cx);
                cell(0.22).child(compact_input(
                    &state,
                    false,
                    !js_trim(&row.model).is_empty() && errors.contains_key(&row.id),
                    locked,
                    &theme,
                ))
            } else {
                cell(0.22).child(
                    div()
                        .when(row.removed, |this| {
                            this.text_color(theme.muted_foreground).line_through()
                        })
                        .child(row.model.clone()),
                )
            }
            .children(error.map(|message| {
                div()
                    .mt_1()
                    .text_size(TEXT_XS.size)
                    .text_color(theme.destructive)
                    .child(message.clone())
            }));

            let mut cells: Vec<AnyElement> = vec![model_cell.into_any_element()];
            if row.removed {
                cells.push(
                    cell(0.78)
                        .text_color(theme.muted_foreground)
                        .child("Automatic pricing after saving")
                        .into_any_element(),
                );
            } else {
                let placeholder = if row.is_new {
                    "Optional"
                } else {
                    alias_cell.placeholder
                };
                let state = self.input(
                    &row.id,
                    Column::Alias,
                    &alias_value,
                    placeholder,
                    window,
                    cx,
                );
                cells.push(
                    cell(0.18)
                        .child(compact_input(&state, false, false, locked, &theme))
                        .into_any_element(),
                );
                if !alias.is_empty() {
                    cells.push(
                        cell(0.6)
                            .text_color(theme.muted_foreground)
                            .child(format!("Counted as {alias}"))
                            .into_any_element(),
                    );
                } else {
                    for field in &USAGE_PRICE_FIELDS {
                        let price_cell = usage_price_cell(&selected, &row.model, field.field);
                        let value = row.values.get(&field.field).cloned().unwrap_or_else(|| {
                            if row.is_new {
                                String::new()
                            } else {
                                price_cell.value.clone()
                            }
                        });
                        let placeholder = if row.is_new {
                            if field.optional { "Input rate" } else { "0.00" }
                        } else {
                            price_cell.placeholder
                        };
                        let state = self.input(
                            &row.id,
                            Column::Price(field.field),
                            &value,
                            placeholder,
                            window,
                            cx,
                        );
                        cells.push(
                            cell(price_width)
                                .child(compact_input(&state, true, false, locked, &theme))
                                .into_any_element(),
                        );
                    }
                }
            }
            let (icon, tooltip, aria) = if row.removed {
                (IconName::RotateCcw, "Undo reset", "undo")
            } else if row.is_new {
                (IconName::X, "Remove row", "remove")
            } else {
                (IconName::RotateCcw, "Reset to automatic", "reset")
            };
            let action_entity = entity.clone();
            let row_clone = row.clone();
            cells.push(
                div()
                    .w(rems(2.5))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        Button::new(SharedString::from(format!("usage-price-{aria}:{}", row.id)))
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::IconXs)
                            .icon(icon)
                            .disabled(locked)
                            .tooltip(Tooltip::new(tooltip))
                            .on_click(move |_, _, cx| {
                                let row = row_clone.clone();
                                action_entity.update(cx, |this, cx| {
                                    if row.is_new {
                                        this.drafts.retain(|entry| entry.id != row.id);
                                        this.attempt = None;
                                    } else if row.removed {
                                        let mut next = row;
                                        next.removed = false;
                                        this.update_or_drop_draft(next);
                                    } else {
                                        let mut next = row;
                                        next.removed = true;
                                        this.update_draft(next);
                                    }
                                    this.reset_inputs();
                                    cx.notify();
                                });
                            }),
                    )
                    .into_any_element(),
            );
            row_elements.push(
                div()
                    .id(SharedString::from(format!("usage-price-row:{}", row.id)))
                    .flex()
                    .items_start()
                    .when(index != last, |this| {
                        this.border_b_1().border_color(theme.border)
                    })
                    .hover({
                        let hover = with_alpha(theme.muted, 0.5);
                        move |style| style.bg(hover)
                    })
                    .children(cells)
                    .into_any_element(),
            );
        }
        if rows.is_empty() {
            row_elements.push(
                div()
                    .flex()
                    .justify_center()
                    .py_6()
                    .px_2()
                    .text_color(theme.muted_foreground)
                    .child(
                        if selected
                            .iter()
                            .any(|environment| environment.prices.is_none())
                        {
                            "Some environment prices are unavailable."
                        } else {
                            "No custom prices or mappings. Add a row to set one."
                        },
                    )
                    .into_any_element(),
            );
        }

        let body = if selected.is_empty() {
            div()
                .text_size(crate::theme::tokens::TEXT_SM.size)
                .text_color(theme.muted_foreground)
                .child(if environments.is_empty() {
                    "Connect an environment to set model prices."
                } else {
                    "Select an environment to see and change its model prices."
                })
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .gap_4()
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .rounded(RADIUS_LG)
                        .border_1()
                        .border_color(theme.border)
                        .text_size(TEXT_XS.size)
                        .child(table_header)
                        .children(row_elements),
                )
                .child(
                    div()
                        .text_size(TEXT_XS.size)
                        .text_color(theme.muted_foreground)
                        .child(format!(
                            "Blank cache rates use the input price. Enter 0 for free tokens. A mapped model’s usage counts as the model it maps to.{}",
                            if selected.len() > 1 {
                                " Mixed cells keep each environment’s rate until you edit them."
                            } else {
                                ""
                            }
                        )),
                )
                .into_any_element()
        };

        let status = self.attempt.as_ref().map(|attempt| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .text_size(TEXT_XS.size)
                .children(attempt.destinations.iter().map(|(id, label)| {
                    let result = attempt.results.get(id);
                    let failed = matches!(
                        result,
                        Some(AttemptResult::Done(UsagePriceWriteResult::Failed { .. }))
                    );
                    div()
                        .flex()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .truncate()
                                .text_color(theme.muted_foreground)
                                .child(label.clone()),
                        )
                        .child(
                            div()
                                .text_color(if failed {
                                    theme.destructive
                                } else {
                                    theme.muted_foreground
                                })
                                .child(match result {
                                    Some(AttemptResult::Done(UsagePriceWriteResult::Failed {
                                        error,
                                    })) => {
                                        format!("Not saved · {error}")
                                    }
                                    Some(AttemptResult::Done(UsagePriceWriteResult::Saved)) => {
                                        "Saved".to_owned()
                                    }
                                    _ => "Saving…".to_owned(),
                                }),
                        )
                }))
        });

        // ── Footer ──────────────────────────────────────────────────────────
        let retrying = !failed_destinations.is_empty();
        let save_disabled = self.pending
            || (!has_changes && !retrying)
            || (!retrying && (!errors.is_empty() || selected.is_empty()));
        let discard_entity = entity.clone();
        let save_entity = entity.clone();
        let footer = dialog_footer(&theme, true)
            .justify_between()
            .child(
                div()
                    .text_size(TEXT_XS.size)
                    .text_color(theme.muted_foreground)
                    .child(if has_changes {
                        format!("Changes apply to {destination_label}")
                    } else {
                        String::new()
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .gap_2()
                    .when(has_changes, |this| {
                        this.child(
                            Button::new("usage-prices-discard")
                                .variant(ButtonVariant::Ghost)
                                .label(if retrying {
                                    "Discard pending changes"
                                } else {
                                    "Discard changes"
                                })
                                .disabled(self.pending)
                                .on_click(move |_, _, cx| {
                                    discard_entity.update(cx, |this, cx| {
                                        this.discard();
                                        cx.notify();
                                    });
                                }),
                        )
                    })
                    .child(
                        Button::new("usage-prices-save")
                            .label(if self.pending {
                                "Saving…"
                            } else if retrying {
                                "Retry failed saves"
                            } else {
                                "Save changes"
                            })
                            .disabled(save_disabled)
                            .on_click(move |_, window, cx| {
                                save_entity.update(cx, |this, cx| this.save(retrying, window, cx));
                            }),
                    ),
            );

        dialog_popup(window, !self.pending)
            .child(header)
            .child(
                dialog_panel("usage-prices-panel", true, true)
                    .child(
                        div()
                            .relative()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .relative()
                                    .flex()
                                    .min_w_0()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .text_size(crate::theme::tokens::TEXT_SM.size)
                                            .font_weight(FontWeight::MEDIUM)
                                            .child("Apply to"),
                                    )
                                    .child(div().relative().child(apply_button).children(menu)),
                            )
                            .child(
                                div()
                                    .text_size(TEXT_XS.size)
                                    .text_color(theme.muted_foreground)
                                    .child("USD / million tokens"),
                            ),
                    )
                    .child(body)
                    .children(status),
            )
            .child(footer)
    }
}
