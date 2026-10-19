//! Port of upstream/apps/web/src/components/settings/ThemeEditorPanel.tsx (the view).
//!
//! The draft and its save rules are `theme_editor_panel.rs`; this is the floating panel
//! that edits them: a draggable, resizable, minimizable window with the name, the
//! light/dark appearance, the guided two-colour mode or the grouped advanced fields, and
//! Cancel / Create. The draft is painted on the live app while the panel is open
//! (`theme_runtime::preview_draft`) and the stored theme returns when it closes.
//!
//! Not ported: the element inspector (`themeInspector.ts`, "Inspect" and the usage
//! spotlight). It picks an element from the DOM with computed styles and a
//! MutationObserver; GPUI has no such tree to read. Selecting a field (clicking its
//! label) still marks it, and the subtitle names it, without a usage count.

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Bounds, ClickEvent, Context, CursorStyle, Entity, FontWeight,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement as _, Pixels, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, canvas, div, px, rems,
};

use super::theme_color_picker::{ThemeColorField, get_theme_role_label};
use super::theme_editor_panel::{
    THEME_EDITOR_ROLE_GROUPS, THEME_EDITOR_SIMPLE_ROLES, ThemeEditorDraft, ThemeEditorRoleGroup,
    ThemeSavedContext, get_theme_editor_color_family,
};
use crate::settings::controls::{FieldSize, switch, text_field};
use crate::theme::color::with_alpha;
use crate::theme::theme_palette_library::get_color_role;
use crate::theme::theme_palettes::{ThemeAppearance, ThemeDefinition};
use crate::theme::tokens::{self, TEXT_SM, TEXT_XS};
use crate::theme::{ActiveAppTheme, theme_runtime};
use crate::ui::command::{dialog_glass_background, dialog_glass_border};
use crate::ui::toggle::{Toggle, ToggleSize, ToggleVariant};
use crate::ui::toggle_group::ToggleGroup;
use crate::ui::{Button, ButtonSize, ButtonVariant, IconName, Tooltip};

// ── Pure geometry (the clamps and the corner grip) ───────────────────

/// `const margin = 8`.
pub const PANEL_MARGIN: f32 = 8.0;
pub const PANEL_MIN_WIDTH: f32 = 280.0;
pub const PANEL_MIN_HEIGHT: f32 = 220.0;
/// `w-[min(26rem,calc(100vw-2rem))]`.
pub fn default_panel_width(viewport_width: f32) -> f32 {
    416.0_f32.min(viewport_width - 32.0)
}

/// `max-h-[min(42rem,calc(100dvh-6rem))]`.
pub fn max_panel_height(viewport_height: f32) -> f32 {
    672.0_f32.min(viewport_height - 96.0)
}

/// `clampPosition`: keeps the panel inside the window, and at least its header on screen
/// however far down it is dragged.
pub fn clamp_panel_position(
    x: f32,
    y: f32,
    width: f32,
    viewport_width: f32,
    viewport_height: f32,
) -> (f32, f32) {
    (
        x.max(PANEL_MARGIN)
            .min(PANEL_MARGIN.max(viewport_width - width - PANEL_MARGIN)),
        y.max(PANEL_MARGIN)
            .min(PANEL_MARGIN.max(viewport_height - 48.0)),
    )
}

/// The window shrank: a chosen size shrinks to fit first, then the position is clamped
/// against it. A panel sized wider than the window could not be pulled back into view by
/// its position alone.
pub fn clamp_panel_size(
    width: f32,
    height: f32,
    viewport_width: f32,
    viewport_height: f32,
) -> (f32, f32) {
    (
        PANEL_MIN_WIDTH.max(width.min(viewport_width - PANEL_MARGIN * 2.0)),
        PANEL_MIN_HEIGHT.max(height.min(viewport_height - PANEL_MARGIN * 2.0)),
    )
}

/// Where a corner-grip drag began.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResizeStart {
    pub pointer: (f32, f32),
    /// The panel's top-left: the grip only moves the opposite corner, so the room to grow
    /// is measured from here.
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
}

/// `handleResizePointerMove`: grows only into the space right of and below the panel's own
/// corner, otherwise a panel parked away from the top-left pushes its far edges (and the
/// grip) off screen.
pub fn resized_panel(
    start: ResizeStart,
    pointer: (f32, f32),
    viewport_width: f32,
    viewport_height: f32,
) -> (f32, f32) {
    let max_width = PANEL_MIN_WIDTH.max(viewport_width - PANEL_MARGIN - start.left);
    let max_height = PANEL_MIN_HEIGHT.max(viewport_height - PANEL_MARGIN - start.top);
    (
        (start.width + pointer.0 - start.pointer.0)
            .max(PANEL_MIN_WIDTH)
            .min(max_width),
        (start.height + pointer.1 - start.pointer.1)
            .max(PANEL_MIN_HEIGHT)
            .min(max_height),
    )
}

/// The header's second line.
pub fn panel_subtitle(draft: &ThemeEditorDraft) -> String {
    match draft.selected_role {
        None => "Select a color below".to_owned(),
        Some(role) => {
            if draft.is_advanced {
                get_theme_editor_color_family(role)
                    .map(|family| family.label.to_owned())
                    .unwrap_or_else(|| get_theme_role_label(role))
            } else {
                get_theme_role_label(role)
            }
        }
    }
}

/// The families of `group` whose label or role labels contain the filter text.
pub fn filtered_families(
    group: &ThemeEditorRoleGroup,
    query: &str,
) -> Vec<&'static super::theme_editor_panel::ThemeEditorColorFamily> {
    let query = query.trim().to_lowercase();
    group
        .families
        .iter()
        .filter(|family| {
            query.is_empty()
                || std::iter::once(family.label.to_owned())
                    .chain(family.roles.iter().map(|role| get_theme_role_label(role)))
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_lowercase()
                    .contains(&query)
        })
        .collect()
}

// ── The view ─────────────────────────────────────────────────────────

/// `onSaved`: the host applies the saved theme and reports whether it could be made active.
pub type SavedHandler =
    Rc<dyn Fn(&ThemeDefinition, ThemeSavedContext, &mut Window, &mut App) -> bool>;
pub type CloseHandler = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(Clone, Copy)]
struct DragState {
    dx: f32,
    dy: f32,
}

pub struct ThemeEditorPanelView {
    draft: ThemeEditorDraft,
    editing_theme: Option<ThemeDefinition>,
    on_saved: SavedHandler,
    on_close: CloseHandler,
    name_input: Entity<InputState>,
    filter_input: Entity<InputState>,
    fields: HashMap<&'static str, Entity<ThemeColorField>>,
    minimized: bool,
    /// `None` parks the panel at its default corner; a value is a dragged spot.
    position: Option<(f32, f32)>,
    /// `None` keeps the responsive default size; a value is a corner-grip resize.
    size: Option<(f32, f32)>,
    drag: Option<DragState>,
    resize: Option<ResizeStart>,
    /// Where the panel painted last (window coordinates).
    bounds: Bounds<Pixels>,
    _subscriptions: Vec<Subscription>,
}

impl ThemeEditorPanelView {
    /// Seeds the draft (`useEffect` on open): editing works on the theme itself; creating
    /// starts from the theme in use.
    pub fn new(
        editing_theme: Option<ThemeDefinition>,
        seed_theme: Option<ThemeDefinition>,
        seed_name: Option<String>,
        initial_appearance: ThemeAppearance,
        on_saved: SavedHandler,
        on_close: CloseHandler,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let draft = ThemeEditorDraft::seeded(
            editing_theme.as_ref(),
            seed_theme.as_ref(),
            seed_name.as_deref(),
            initial_appearance,
        );
        let is_editing = editing_theme.is_some();
        let name = draft.name.clone();
        let name_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(if is_editing {
                    "Theme name"
                } else {
                    "e.g. Aurora"
                })
                .default_value(name)
        });
        let filter_input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter colors"));
        let subscriptions = vec![
            cx.subscribe_in(
                &name_input,
                window,
                |this, input, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Change) {
                        let text = input.read(cx).value().to_string();
                        if text != this.draft.name {
                            // Most save failures are name collisions; retyping is the fix, so
                            // the stale message goes with the old name.
                            this.draft.set_name(text, this.editing_theme.as_ref());
                            this.changed(window, cx);
                        }
                    }
                },
            ),
            cx.subscribe_in(
                &filter_input,
                window,
                |this, input, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.draft.role_query = input.read(cx).value().to_string();
                        cx.notify();
                    }
                },
            ),
        ];
        let mut this = Self {
            draft,
            editing_theme,
            on_saved,
            on_close,
            name_input,
            filter_input,
            fields: HashMap::new(),
            minimized: false,
            position: None,
            size: None,
            drag: None,
            resize: None,
            bounds: Bounds::default(),
            _subscriptions: subscriptions,
        };
        // The draft only reaches the live app once this open has been seeded.
        this.preview(cx);
        this
    }

    /// The library changed under an open session (`useThemeDefinition`): the theme being
    /// edited may have been saved elsewhere or removed.
    pub fn set_editing_theme(
        &mut self,
        editing_theme: Option<ThemeDefinition>,
        cx: &mut Context<Self>,
    ) {
        if self.editing_theme != editing_theme {
            self.editing_theme = editing_theme;
            cx.notify();
        }
    }

    pub fn draft(&self) -> &ThemeEditorDraft {
        &self.draft
    }

    /// The whole app wears the draft while the editor is open, so a role change is judged on
    /// the real interface rather than a miniature.
    fn preview(&self, cx: &mut App) {
        let (colors, appearance) = self.draft.preview();
        theme_runtime::preview_draft(colors, appearance, cx);
    }

    /// The stored theme comes back (`restoreTheme` on close).
    pub fn end_preview(&self, cx: &mut App) {
        theme_runtime::end_preview(cx);
    }

    fn changed(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.preview(cx);
        cx.notify();
    }

    fn update_color(&mut self, role: &str, value: &str, cx: &mut Context<Self>) {
        self.draft.update_color(role, value);
        self.preview(cx);
        cx.notify();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        (self.on_close)(window, cx);
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let on_saved = self.on_saved.clone();
        let editing = self.editing_theme.clone();
        let closed = {
            let mut saved = |theme: &ThemeDefinition, context: ThemeSavedContext| {
                on_saved(theme, context, window, cx)
            };
            self.draft.submit(editing.as_ref(), &mut saved)
        };
        if closed {
            self.close(window, cx);
        } else {
            cx.notify();
        }
    }

    // The field for `role`, created once with its label.
    fn field(
        &mut self,
        role: &'static str,
        label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<ThemeColorField> {
        if let Some(field) = self.fields.get(role) {
            return field.clone();
        }
        let value = get_color_role(self.draft.colors(), role)
            .map(ToString::to_string)
            .unwrap_or_default();
        let panel = cx.entity().downgrade();
        let on_change: Rc<dyn Fn(&str, &str, &mut App)> = {
            let panel = panel.clone();
            Rc::new(move |role, value, cx| {
                panel
                    .update(cx, |this, cx| this.update_color(role, value, cx))
                    .ok();
            })
        };
        let on_select: Rc<dyn Fn(&str, &mut App)> = {
            let panel = panel.clone();
            Rc::new(move |role, cx| {
                panel
                    .update(cx, |this, cx| {
                        this.draft.select_theme_role(role);
                        cx.notify();
                    })
                    .ok();
            })
        };
        let on_toggle: Rc<dyn Fn(&str, &mut App)> = Rc::new(move |role, cx| {
            panel
                .update(cx, |this, cx| {
                    this.draft.toggle_theme_role(role);
                    cx.notify();
                })
                .ok();
        });
        let label = label.to_owned();
        let field = cx.new(|cx| {
            ThemeColorField::new(
                role,
                Some(&label),
                &value,
                on_change,
                Some(on_select),
                Some(on_toggle),
                window,
                cx,
            )
        });
        self.fields.insert(role, field.clone());
        field
    }

    /// One row of colour fields, brought up to date with the draft.
    fn field_row(
        &mut self,
        role: &'static str,
        label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let field = self.field(role, label, window, cx);
        let value = get_color_role(self.draft.colors(), role)
            .map(ToString::to_string)
            .unwrap_or_default();
        let selected = self.draft.selected_role == Some(role);
        field.update(cx, |field, cx| {
            field.update_props(&value, selected, window, cx)
        });
        field.into_any_element()
    }

    fn appearance_toggle(&self, appearance: ThemeAppearance, cx: &mut Context<Self>) -> AnyElement {
        let merge_target = self.draft.merge_target(self.editing_theme.as_ref());
        let lock = self.draft.appearance_lock_reason(
            appearance,
            self.editing_theme.as_ref(),
            merge_target.as_ref(),
        );
        let pressed = self.draft.active_appearance == appearance;
        let label = if appearance == ThemeAppearance::Light {
            "Light"
        } else {
            "Dark"
        };
        let id = SharedString::from(format!("theme-editor-appearance-{label}"));
        let toggle = Toggle::new(id)
            .variant(ToggleVariant::Segmented)
            .size(ToggleSize::Segmented)
            .label(label)
            .pressed(pressed)
            // A locked mode stays hoverable so the tooltip can say why it is off.
            .when_some(lock.clone(), |toggle, reason| {
                toggle.tooltip(Tooltip::new(reason))
            })
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                if this
                    .draft
                    .set_active_appearance(appearance, this.editing_theme.as_ref())
                {
                    this.changed(window, cx);
                }
            }));
        toggle.into_any_element()
    }

    fn colors_header(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let advanced = self.draft.is_advanced;
        let panel = cx.entity().downgrade();
        div()
            .flex()
            .gap_3()
            .items_start()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(TEXT_SM.size)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.foreground)
                            .child("Colors"),
                    )
                    .when(!advanced, |this| {
                        this.child(
                            div()
                                .text_size(TEXT_XS.size)
                                .text_color(theme.muted_foreground)
                                .child("Two colors, rest derived"),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_start()
                    .gap_3()
                    .when(advanced, |this| {
                        this.child(div().flex_1().min_w_0().child(text_field(
                            &self.filter_input,
                            None,
                            FieldSize::Sm,
                            false,
                            cx,
                        )))
                    })
                    .child(
                        div()
                            .ml_auto()
                            .flex()
                            .flex_shrink_0()
                            .items_center()
                            .gap_2()
                            .pt(px(2.))
                            .text_size(TEXT_SM.size)
                            .font_weight(FontWeight::MEDIUM)
                            .child("Advanced")
                            .child(switch(
                                "theme-editor-advanced",
                                advanced,
                                false,
                                cx,
                                move |checked, window, cx| {
                                    panel
                                        .update(cx, |this, cx| {
                                            this.draft.handle_advanced_change(
                                                checked,
                                                this.editing_theme.as_ref(),
                                            );
                                            this.changed(window, cx);
                                        })
                                        .ok();
                                },
                            )),
                    ),
            )
            .into_any_element()
    }

    fn color_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        if !self.draft.is_advanced {
            let rows: Vec<AnyElement> = THEME_EDITOR_SIMPLE_ROLES
                .iter()
                .map(|role| {
                    let label = if *role == "canvas" {
                        "Background"
                    } else {
                        "Accent"
                    };
                    self.field_row(role, label, window, cx)
                })
                .collect();
            return div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .children(rows)
                .into_any_element();
        }
        let query = self.draft.role_query.clone();
        let mut sections = Vec::new();
        for group in THEME_EDITOR_ROLE_GROUPS {
            let families = filtered_families(group, &query);
            if families.is_empty() {
                continue;
            }
            let rows: Vec<AnyElement> = families
                .into_iter()
                .map(|family| self.field_row(family.role, family.label, window, cx))
                .collect();
            sections.push(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_size(TEXT_SM.size)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.foreground)
                            .child(group.title),
                    )
                    .child(div().flex().flex_col().gap(px(4.)).children(rows))
                    .into_any_element(),
            );
        }
        if sections.is_empty() {
            sections.push(
                div()
                    .text_size(TEXT_XS.size)
                    .text_color(theme.muted_foreground)
                    .child("No matches.")
                    .into_any_element(),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap_5()
            .children(sections)
            .into_any_element()
    }

    fn header(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let title = if self.editing_theme.is_some() {
            "Edit theme"
        } else {
            "Create theme"
        };
        let minimized = self.minimized;
        let subtitle = panel_subtitle(&self.draft);
        // `flex cursor-grab select-none items-center gap-1 border-b border-border/70 px-3 py-2`
        div()
            .id("theme-editor-header")
            .flex()
            .items_center()
            .gap(px(4.))
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(with_alpha(theme.border, 0.7))
            .cursor(CursorStyle::OpenHand)
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| {
                    this.drag = None;
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.drag = Some(DragState {
                        dx: f32::from(event.position.x - this.bounds.origin.x),
                        dy: f32::from(event.position.y - this.bounds.origin.y),
                    });
                    cx.notify();
                }),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_baseline()
                    .gap_2()
                    .child(
                        div()
                            .flex_shrink_0()
                            .truncate()
                            .text_size(TEXT_SM.size)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.foreground)
                            .child(title),
                    )
                    .when(!minimized, |this| {
                        this.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(TEXT_XS.size)
                                .text_color(theme.muted_foreground)
                                .child(SharedString::from(subtitle)),
                        )
                    }),
            )
            .child(
                Button::new("theme-editor-minimize")
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::IconXs)
                    .icon(if minimized {
                        IconName::ChevronUp
                    } else {
                        IconName::ChevronDown
                    })
                    .tooltip(Tooltip::new(if minimized {
                        "Expand the theme editor"
                    } else {
                        "Minimize the theme editor"
                    }))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.minimized = !this.minimized;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("theme-editor-close")
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::IconXs)
                    .icon(IconName::X)
                    .tooltip(Tooltip::new("Close the theme editor"))
                    .on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx)),
                    ),
            )
            .into_any_element()
    }

    fn body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let label = |text: &'static str| {
            div()
                .text_size(TEXT_SM.size)
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.foreground)
                .child(text)
        };
        // `grid grid-cols-[minmax(0,1fr)_minmax(0,2fr)] items-center gap-3`
        let name_row = div()
            .flex()
            .items_center()
            .gap_3()
            .child(div().flex_1().min_w_0().child(label("Theme name")))
            .child(
                div()
                    .flex_grow(2.)
                    .flex_basis(px(0.))
                    .min_w_0()
                    .child(text_field(&self.name_input, None, FieldSize::Sm, false, cx)),
            );
        let appearance_row = div()
            .flex()
            .items_center()
            .gap_3()
            .child(div().flex_1().min_w_0().child(label("Appearance")))
            .child(
                div().flex_grow(2.).flex_basis(px(0.)).min_w_0().child(
                    ToggleGroup::new()
                        .child(self.appearance_toggle(ThemeAppearance::Light, cx))
                        .child(self.appearance_toggle(ThemeAppearance::Dark, cx)),
                ),
            );
        let header = self.colors_header(cx);
        let fields = self.color_fields(window, cx);
        // `min-h-0 flex-1 space-y-3 overflow-y-auto py-3 pl-3 pr-1.5`
        div()
            .id("theme-editor-body")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap_3()
            .overflow_y_scroll()
            .py_3()
            .pl_3()
            .pr(px(6.))
            .child(name_row)
            // Inline and above the color list: the panel scrolls, and an error parked below
            // every role would go unseen.
            .children(self.draft.error.clone().map(|error| {
                div()
                    .text_size(TEXT_SM.size)
                    .text_color(theme.destructive)
                    .child(SharedString::from(error))
            }))
            .child(appearance_row)
            .child(div().flex().flex_col().gap_3().child(header).child(fields))
            .into_any_element()
    }

    fn footer(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let merge_target = self.draft.merge_target(self.editing_theme.as_ref());
        let label = self
            .draft
            .submit_label(self.editing_theme.as_ref(), merge_target.as_ref());
        let icon = match (self.editing_theme.is_some(), merge_target.is_some()) {
            (false, true) => Some(IconName::Plus),
            (false, false) => Some(IconName::Palette),
            _ => None,
        };
        div()
            .flex()
            .items_center()
            .justify_end()
            .gap_2()
            .border_t_1()
            .border_color(with_alpha(theme.border, 0.7))
            .px_3()
            .py_2()
            .child(
                Button::new("theme-editor-cancel")
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Sm)
                    .label("Cancel")
                    .on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx)),
                    ),
            )
            .child(
                Button::new("theme-editor-submit")
                    .size(ButtonSize::Sm)
                    .disabled(self.draft.name.trim().is_empty())
                    .when_some(icon, |button, icon| button.icon(icon))
                    .label(label)
                    .on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| this.submit(window, cx)),
                    ),
            )
            .into_any_element()
    }

    /// The corner grip: `absolute bottom-0 right-0 z-10 flex size-5 cursor-se-resize`.
    fn grip(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        div()
            .id("theme-editor-grip")
            .absolute()
            .bottom_0()
            .right_0()
            .flex()
            .size(rems(1.25))
            .items_end()
            .justify_end()
            .p(px(4.))
            .cursor(CursorStyle::ResizeUpLeftDownRight)
            .text_color(with_alpha(theme.muted_foreground, 0.7))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    let viewport = window.viewport_size();
                    let (vw, vh) = (f32::from(viewport.width), f32::from(viewport.height));
                    let origin = this.bounds.origin;
                    // The default spot is anchored bottom-right and would slide as the panel
                    // grows, so it converts to an explicit position first.
                    let width = f32::from(this.bounds.size.width);
                    if this.position.is_none() {
                        this.position = Some(clamp_panel_position(
                            f32::from(origin.x),
                            f32::from(origin.y),
                            width,
                            vw,
                            vh,
                        ));
                    }
                    this.resize = Some(ResizeStart {
                        pointer: (f32::from(event.position.x), f32::from(event.position.y)),
                        left: f32::from(origin.x),
                        top: f32::from(origin.y),
                        width,
                        height: f32::from(this.bounds.size.height),
                    });
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .child(
                // Two short diagonal strokes (`M7 1 1 7M7 4.5 4.5 7`).
                div()
                    .size(px(10.))
                    .border_b_1()
                    .border_r_1()
                    .border_color(with_alpha(theme.muted_foreground, 0.7)),
            )
            .into_any_element()
    }
}

impl Render for ThemeEditorPanelView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let viewport = window.viewport_size();
        let (vw, vh) = (f32::from(viewport.width), f32::from(viewport.height));
        let panel_entity = cx.entity();
        // A window resize pulls a too-large panel back into view.
        if let Some((width, height)) = self.size {
            self.size = Some(clamp_panel_size(width, height, vw, vh));
        }
        let width = self
            .size
            .map_or_else(|| default_panel_width(vw), |(width, _)| width);
        if let Some((x, y)) = self.position {
            self.position = Some(clamp_panel_position(x, y, width, vw, vh));
        }

        let mut panel = div()
            .id("theme-editor-panel")
            .absolute()
            .flex()
            .flex_col()
            .overflow_hidden()
            .w(px(width))
            .rounded(tokens::RADIUS_XL)
            .border_1()
            .border_color(dialog_glass_border(&theme))
            .bg(dialog_glass_background(&theme))
            .text_color(theme.popover_foreground)
            .shadow(tokens::shadow_dialog(theme.is_dark()))
            // The panel is a window of its own: a click on it is not a click on the page.
            .occlude();
        panel = match self.position {
            Some((x, y)) => panel.left(px(x)).top(px(y)),
            None => panel.bottom(px(16.)).right(px(16.)),
        };
        panel = match (self.size, self.minimized) {
            (Some((_, height)), false) => panel.h(px(height)).max_h(px(vh - 16.)),
            _ => panel.max_h(px(if self.minimized {
                f32::MAX
            } else {
                max_panel_height(vh)
            })),
        };
        let header = self.header(cx);
        let content = if self.minimized {
            Vec::new()
        } else {
            vec![self.body(window, cx), self.footer(cx), self.grip(cx)]
        };
        let measure = canvas(
            {
                let panel = panel_entity.clone();
                move |bounds, _, cx| {
                    if panel.read(cx).bounds != bounds {
                        panel.update(cx, |this, _| this.bounds = bounds);
                    }
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();

        // While the header or the grip is held, a transparent layer over the window follows the
        // pointer, so a fast drag cannot outrun the panel.
        let gesture = (self.drag.is_some() || self.resize.is_some()).then(|| {
            div()
                .id("theme-editor-gesture")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .cursor(if self.resize.is_some() {
                    CursorStyle::ResizeUpLeftDownRight
                } else {
                    CursorStyle::ClosedHand
                })
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                    let viewport = window.viewport_size();
                    let (vw, vh) = (f32::from(viewport.width), f32::from(viewport.height));
                    if let Some(start) = this.resize {
                        this.size = Some(resized_panel(
                            start,
                            (f32::from(event.position.x), f32::from(event.position.y)),
                            vw,
                            vh,
                        ));
                    } else if let Some(drag) = this.drag {
                        let width = this
                            .size
                            .map_or_else(|| f32::from(this.bounds.size.width), |(width, _)| width);
                        this.position = Some(clamp_panel_position(
                            f32::from(event.position.x) - drag.dx,
                            f32::from(event.position.y) - drag.dy,
                            width,
                            vw,
                            vh,
                        ));
                    }
                    cx.notify();
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseUpEvent, _, cx| {
                        this.drag = None;
                        this.resize = None;
                        cx.notify();
                    }),
                )
        });

        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .child(panel.child(measure).child(header).children(content))
            .children(gesture)
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn the_panel_is_26rem_wide_unless_the_window_is_narrower() {
        assert_eq!(default_panel_width(1200.0), 416.0);
        assert_eq!(default_panel_width(400.0), 368.0);
    }

    #[test]
    fn the_panel_is_at_most_42rem_tall_and_leaves_6rem_of_the_window() {
        assert_eq!(max_panel_height(1200.0), 672.0);
        assert_eq!(max_panel_height(600.0), 504.0);
    }

    #[test]
    fn a_dragged_position_stays_in_the_window_with_its_header_reachable() {
        // Inside: unchanged.
        assert_eq!(
            clamp_panel_position(100.0, 100.0, 416.0, 1200.0, 800.0),
            (100.0, 100.0)
        );
        // Off the left and top edges: the 8px margin.
        assert_eq!(
            clamp_panel_position(-50.0, -20.0, 416.0, 1200.0, 800.0),
            (8.0, 8.0)
        );
        // Off the right: the right edge keeps the margin.
        assert_eq!(
            clamp_panel_position(1000.0, 100.0, 416.0, 1200.0, 800.0).0,
            1200.0 - 416.0 - 8.0
        );
        // Far down: only the 48px header stays on screen.
        assert_eq!(
            clamp_panel_position(100.0, 5000.0, 416.0, 1200.0, 800.0).1,
            800.0 - 48.0
        );
        // A panel wider than the window sits at the margin.
        assert_eq!(
            clamp_panel_position(300.0, 100.0, 2000.0, 1200.0, 800.0).0,
            8.0
        );
    }

    #[test]
    fn a_chosen_size_shrinks_to_fit_a_smaller_window_but_not_below_the_minimum() {
        assert_eq!(
            clamp_panel_size(600.0, 500.0, 1200.0, 800.0),
            (600.0, 500.0)
        );
        assert_eq!(
            clamp_panel_size(1500.0, 900.0, 1200.0, 800.0),
            (1184.0, 784.0)
        );
        assert_eq!(
            clamp_panel_size(100.0, 100.0, 1200.0, 800.0),
            (280.0, 220.0)
        );
        assert_eq!(clamp_panel_size(900.0, 900.0, 200.0, 200.0), (280.0, 220.0));
    }

    #[test]
    fn the_grip_grows_the_panel_only_into_the_room_right_of_and_below_its_corner() {
        let start = ResizeStart {
            pointer: (500.0, 400.0),
            left: 300.0,
            top: 100.0,
            width: 416.0,
            height: 500.0,
        };
        // Drag right and down by 50.
        assert_eq!(
            resized_panel(start, (550.0, 450.0), 1200.0, 800.0),
            (466.0, 550.0)
        );
        // Drag far: capped at the window edge minus the margin.
        assert_eq!(
            resized_panel(start, (5000.0, 5000.0), 1200.0, 800.0),
            (1200.0 - 8.0 - 300.0, 800.0 - 8.0 - 100.0)
        );
        // Drag back far: the minimum size.
        assert_eq!(
            resized_panel(start, (0.0, 0.0), 1200.0, 800.0),
            (280.0, 220.0)
        );
    }

    #[test]
    fn the_filter_matches_family_and_role_labels_case_insensitively() {
        let foundation = THEME_EDITOR_ROLE_GROUPS
            .iter()
            .find(|group| group.id == "foundation")
            .expect("foundation group");
        assert_eq!(
            filtered_families(foundation, "").len(),
            foundation.families.len()
        );
        // "Toolbar text" is a role label inside the Text family.
        let ids: Vec<&str> = filtered_families(foundation, "  TOOLBAR text ")
            .iter()
            .map(|family| family.id)
            .collect();
        assert_eq!(ids, vec!["text"]);
        assert!(filtered_families(foundation, "no such colour").is_empty());
    }

    #[test]
    fn the_subtitle_names_the_selected_color_or_asks_for_one() {
        let mut draft = ThemeEditorDraft::seeded(None, None, None, ThemeAppearance::Dark);
        assert_eq!(panel_subtitle(&draft), "Select a color below");
        draft.select_theme_role("accent");
        assert_eq!(panel_subtitle(&draft), "Accent color");
        // Advanced reads the family label.
        draft.select_theme_role("surface");
        assert!(draft.is_advanced);
        assert_eq!(panel_subtitle(&draft), "Surface");
    }
}
