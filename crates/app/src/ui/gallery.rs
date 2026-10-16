//! Dev gallery for the S3 primitives (no upstream counterpart).
//!
//! `TURIN_UI_GALLERY=<page>` opens this view instead of the app shell, the
//! way `TURIN_PULL_REQUESTS_DEMO` swaps in the pull request views. Pages:
//! `controls`, `data`, `overlays`, `popups`, `toasts`, `toasts-expanded`. `1` or `all`
//! is `controls`. Every primitive renders in its main states; `scripts/shot.sh`
//! captures each page in light and dark.

use chrono::NaiveDate;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Bounds, ClickEvent, Context, Entity, FontWeight, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, Styled, Window, div, point, px, rems, size,
};

use super::alert_dialog::{
    AlertDialogFooterVariant, alert_dialog_description, alert_dialog_footer, alert_dialog_header,
    alert_dialog_title,
};
use super::animated_size::{AnimatedHeight, AnimatedWidth};
use super::autocomplete::{AutocompleteItem, AutocompletePopup, autocomplete_group_label};
use super::button::{Button, ButtonSize, ButtonVariant};
use super::calendar::Calendar;
use super::collapsible::Collapsible;
use super::color_picker::{ColorHueSlider, ColorSaturationValuePlane, HsvColor, PlaneVariant};
use super::combobox::{
    ComboboxItem, ComboboxPopup, combobox_empty, combobox_search_input, combobox_status,
};
use super::discovery_list::{DiscoveryList, DiscoveryListRow};
use super::group::Group;
use super::icon::{Icon, IconName};
use super::input_group::{InputGroup, InputGroupAddon, InputGroupAlign};
use super::label::Label;
use super::morph_icon::MorphIcon;
use super::number_field::{NumberField, NumberFieldSize, ValidateOptions};
use super::panel_tab_close_button::{PanelTabCloseButton, TAB_GROUP};
use super::preview_card::{PreviewCard, PreviewSide};
use super::radio_group::{Radio, RadioGroup};
use super::refresh_icon::{RefreshIcon, RefreshIconSize};
use super::select::{
    SelectButton, SelectItem, SelectPopup, select_ghost_trigger, select_group_label,
};
use super::sheet::{Sheet, SheetSide, sheet_description, sheet_header, sheet_title};
use super::start_truncated_path::StartTruncatedPath;
use super::switch::{Switch, SwitchSize};
use super::t3_wordmark::T3Wordmark;
use super::table::{Table, TableRow, table_cell, table_head};
use super::text_input::text_input_frame;
use super::textarea::{TextareaSize, textarea_frame};
use super::toast::{
    AnchoredToastViewport, ThreadToastData, ToastActionSpec, ToastAnchor, ToastOptions,
    ToastViewport, anchored_toast_manager, anchored_toasts, toast_manager, toasts,
};
use super::toast_logic::{ToastActionLayout, ToastType};
use super::toggle::{Toggle, ToggleSize, ToggleVariant};
use super::toggle_group::ToggleGroup;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_LG, TEXT_SM, TEXT_XS};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GalleryPage {
    Controls,
    Data,
    Overlays,
    Popups,
    Toasts,
    ToastsExpanded,
}

/// `TURIN_UI_GALLERY`: the page to show, if the variable is set.
pub fn page_from_env() -> Option<GalleryPage> {
    let value = std::env::var("TURIN_UI_GALLERY").ok()?;
    Some(match value.as_str() {
        "data" => GalleryPage::Data,
        "overlays" => GalleryPage::Overlays,
        "popups" => GalleryPage::Popups,
        "toasts" => GalleryPage::Toasts,
        "toasts-expanded" => GalleryPage::ToastsExpanded,
        _ => GalleryPage::Controls,
    })
}

pub struct UiGallery {
    page: GalleryPage,
    switch_on: bool,
    radio: usize,
    toggles: [bool; 3],
    quantity: Option<f64>,
    month: (i32, u32),
    selected_day: NaiveDate,
    color: HsvColor,
    collapsible_open: bool,
    grow_content: bool,
    sheet_open: bool,
    toast_viewport: Entity<ToastViewport>,
    anchored_viewport: Entity<AnchoredToastViewport>,
}

impl UiGallery {
    pub fn new(page: GalleryPage, window: &mut Window, cx: &mut Context<Self>) -> Self {
        if matches!(page, GalleryPage::Toasts | GalleryPage::ToastsExpanded) {
            seed_toasts(page == GalleryPage::ToastsExpanded, window, cx);
        }
        Self {
            page,
            switch_on: true,
            radio: 1,
            toggles: [true, false, false],
            quantity: Some(3.),
            month: (2026, 10),
            selected_day: NaiveDate::from_ymd_opt(2026, 10, 15).unwrap_or_default(),
            color: HsvColor {
                h: 210.,
                s: 0.7,
                v: 0.85,
            },
            collapsible_open: true,
            grow_content: false,
            sheet_open: false,
            toast_viewport: toasts(cx),
            anchored_viewport: anchored_toasts(cx),
        }
    }
}

/// Window-relative origin and size of the anchor box the anchored toast sits under.
type Xy = (gpui_kit::Pixels, gpui_kit::Pixels);
const ANCHOR_BOUNDS: (Xy, Xy) = ((px(120.), px(180.)), (px(120.), px(32.)));

fn seed_toasts(expanded: bool, _: &mut Window, cx: &mut Context<UiGallery>) {
    let stack = toast_manager(cx);
    let anchored = anchored_toast_manager(cx);
    stack.update(cx, |store, cx| {
        store.add(
            ToastOptions::new("Settings saved")
                .type_(ToastType::Success)
                .description("Your changes were written to disk.")
                .timeout(0),
            cx,
        );
        store.add(
            ToastOptions::new("Update available")
                .type_(ToastType::Info)
                .description("T3 Code 0.0.42 is ready to install.")
                .action(ToastActionSpec::new("update", "Restart"))
                .data(ThreadToastData {
                    action_layout: Some(ToastActionLayout::StackedEnd),
                    ..ThreadToastData::default()
                })
                .timeout(0),
            cx,
        );
        store.add(
            ToastOptions::new("Could not start the provider")
                .type_(ToastType::Error)
                .description("spawn codex ENOENT: the executable was not found on PATH. Install the CLI or set its path in Settings.")
                .timeout(0),
            cx,
        );
        if expanded {
            store.pin_expanded(cx);
        }
    });
    anchored.update(cx, |store, cx| {
        store.add(
            ToastOptions::new("Copied to clipboard")
                .type_(ToastType::Success)
                .timeout(0)
                .anchor(ToastAnchor {
                    bounds: Bounds::new(
                        point(ANCHOR_BOUNDS.0.0, ANCHOR_BOUNDS.0.1),
                        size(ANCHOR_BOUNDS.1.0, ANCHOR_BOUNDS.1.1),
                    ),
                    side_offset: 4.,
                }),
            cx,
        );
    });
}

fn section(title: &'static str, cx: &App, body: impl IntoElement) -> impl IntoElement {
    let theme = cx.app_theme();
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_size(TEXT_XS.size)
                .line_height(TEXT_XS.line_height)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.muted_foreground)
                .child(SharedString::from(title.to_uppercase())),
        )
        .child(body)
}

fn row() -> gpui_kit::Div {
    div().flex().flex_wrap().items_center().gap_3()
}

fn labelled(text: &'static str, control: impl IntoElement) -> impl IntoElement {
    row()
        .gap_2()
        .child(control)
        .child(Label::new(text).into_any_element())
}

impl Render for UiGallery {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let page = match self.page {
            GalleryPage::Controls => self.controls(cx),
            GalleryPage::Data => self.data(cx),
            GalleryPage::Overlays => self.overlays(cx),
            GalleryPage::Popups => self.popups(window, cx),
            GalleryPage::Toasts | GalleryPage::ToastsExpanded => self.toasts_page(cx),
        };
        let title = match self.page {
            GalleryPage::Controls => "Controls",
            GalleryPage::Data => "Data and layout",
            GalleryPage::Overlays => "Pickers and collapsible",
            GalleryPage::Popups => "Popups and dialogs",
            GalleryPage::Toasts => "Toasts (collapsed)",
            GalleryPage::ToastsExpanded => "Toasts (expanded)",
        };
        let is_toasts = matches!(self.page, GalleryPage::Toasts | GalleryPage::ToastsExpanded);
        let stack = self.toast_viewport.clone();
        let anchored = self.anchored_viewport.clone();
        div()
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(theme.background)
            .text_color(theme.foreground)
            .text_size(TEXT_SM.size)
            .line_height(TEXT_SM.line_height)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_5()
                    .size_full()
                    .px_6()
                    .pt(px(52.))
                    .pb_6()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_size(rems(1.125))
                            .child(SharedString::from(format!("S3 primitives: {title}"))),
                    )
                    .child(page),
            )
            .when(is_toasts, |root| {
                // Window-level so its bounds match the seeded anchor
                // (`ANCHOR_BOUNDS`), not the padded page column.
                root.child(
                    div()
                        .absolute()
                        .top(ANCHOR_BOUNDS.0.1)
                        .left(ANCHOR_BOUNDS.0.0)
                        .w(ANCHOR_BOUNDS.1.0)
                        .h(ANCHOR_BOUNDS.1.1)
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(RADIUS_LG)
                        .border_1()
                        .border_color(theme.border)
                        .child("Anchor"),
                )
            })
            .child(stack)
            .child(anchored)
    }
}

impl UiGallery {
    fn controls(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let switch_on = self.switch_on;
        let radio = self.radio;
        let toggles = self.toggles;
        let quantity = self.quantity;
        let listener = cx.listener(|this, _: &ClickEvent, _, cx| {
            this.switch_on = !this.switch_on;
            cx.notify();
        });
        let toggle_item = |index: usize, label: &'static str, cx: &mut Context<Self>| {
            Toggle::new(SharedString::from(format!("toggle-{index}")))
                .variant(ToggleVariant::Segmented)
                .size(ToggleSize::Segmented)
                .label(label)
                .pressed(toggles[index])
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.toggles = [false; 3];
                    this.toggles[index] = true;
                    cx.notify();
                }))
        };
        let toggle_group = ToggleGroup::new()
            .child(toggle_item(0, "Day", cx))
            .child(toggle_item(1, "Week", cx))
            .child(toggle_item(2, "Month", cx));
        let radios = RadioGroup::new().children((0..3).map(|index| {
            let label = ["Local", "Worktree", "Remote"][index];
            labelled(
                label,
                Radio::new(SharedString::from(format!("radio-{index}")))
                    .checked(radio == index)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.radio = index;
                        cx.notify();
                    })),
            )
        }));
        let this = cx.entity();
        div()
            .grid()
            .grid_cols(2)
            .gap_x_8()
            .gap_y_5()
            .child(section(
                "Switch",
                cx,
                row()
                    .child(Switch::new("sw-on").checked(switch_on).on_click(listener))
                    .child(Switch::new("sw-off").checked(false))
                    .child(Switch::new("sw-sm").size(SwitchSize::Sm).checked(true))
                    .child(Switch::new("sw-mixed").mixed(true))
                    .child(Switch::new("sw-disabled").checked(true).disabled(true)),
            ))
            .child(section("Radio group", cx, radios))
            .child(section("Toggle group", cx, toggle_group))
            .child(section(
                "Number field",
                cx,
                row()
                    .child(
                        div().w(px(132.)).child(
                            NumberField::new("nf-default")
                                .value(quantity)
                                .options(ValidateOptions {
                                    min: Some(0.),
                                    max: Some(10.),
                                    ..ValidateOptions::default()
                                })
                                .on_change(move |value, _, cx| {
                                    this.update(cx, |this, cx| {
                                        this.quantity = value;
                                        cx.notify();
                                    });
                                }),
                        ),
                    )
                    .child(
                        div().w(px(120.)).child(
                            NumberField::new("nf-sm")
                                .size(NumberFieldSize::Sm)
                                .value(Some(1.5)),
                        ),
                    )
                    .child(
                        div().w(px(120.)).child(
                            NumberField::new("nf-disabled")
                                .value(Some(8.))
                                .disabled(true),
                        ),
                    ),
            ))
            .child(section(
                "Textarea",
                cx,
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        textarea_frame(TextareaSize::Default, cx)
                            .child("Describe the change you want"),
                    )
                    .child(textarea_frame(TextareaSize::Sm, cx).child("Small")),
            ))
            .child(section(
                "Input group",
                cx,
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        InputGroup::new()
                            .child(
                                InputGroupAddon::new(InputGroupAlign::InlineStart)
                                    .child(Icon::new(IconName::Search).size(rems(1.))),
                            )
                            .child(div().flex_1().pl_2().child("Search threads")),
                    )
                    .child(
                        div()
                            .w_full()
                            .child(text_input_frame(cx).child("Plain input")),
                    ),
            ))
            .child(section(
                "Group",
                cx,
                Group::horizontal()
                    .child(
                        Button::new("g1")
                            .variant(ButtonVariant::Outline)
                            .label("Copy"),
                    )
                    .child(
                        Button::new("g2")
                            .variant(ButtonVariant::Outline)
                            .label("Paste"),
                    )
                    .child(
                        Button::new("g3")
                            .variant(ButtonVariant::Outline)
                            .label("Clear"),
                    ),
            ))
            .child(section(
                "Select",
                cx,
                div()
                    .relative()
                    .h(px(190.))
                    .child(SelectButton::new("GPT-5.5"))
                    .child(
                        SelectPopup::new()
                            .open(true)
                            .child(select_group_label("Models", cx))
                            .children(
                                ["GPT-5.5", "Claude Sonnet 5.5", "Composer 2.5"]
                                    .into_iter()
                                    .enumerate()
                                    .map(|(index, name)| {
                                        SelectItem::new(name, name)
                                            .selected(index == 0)
                                            .highlighted(index == 1)
                                    }),
                            ),
                    ),
            ))
            .into_any_element()
    }

    fn data(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let rows = [
            ("main", "Up to date", "2 minutes ago"),
            ("feature/gallery", "3 ahead", "Yesterday"),
            ("fix/toast-swipe", "1 behind", "Last week"),
        ];
        let table =
            Table::new()
                .child(
                    TableRow::header()
                        .child(table_head("Branch", cx))
                        .child(table_head("Status", cx))
                        .child(table_head("Updated", cx)),
                )
                .children(rows.into_iter().enumerate().map(
                    |(index, (branch, status, updated))| {
                        TableRow::body()
                            .selected(index == 1)
                            .last(index == rows.len() - 1)
                            .child(table_cell(branch))
                            .child(table_cell(status))
                            .child(table_cell(updated))
                    },
                ));
        let discovery = DiscoveryList::new()
            .child(
                DiscoveryListRow::new(
                    "dl-1",
                    Icon::new(IconName::Bot).size(rems(1.)),
                    "Codex",
                    "OpenAI coding agent",
                )
                .action(
                    Button::new("dl-1-action")
                        .size(ButtonSize::Xs)
                        .label("Connect"),
                ),
            )
            .child(
                DiscoveryListRow::new(
                    "dl-2",
                    Icon::new(IconName::Brain).size(rems(1.)),
                    "Claude",
                    "Anthropic coding agent",
                )
                .divided(true)
                .action(
                    Button::new("dl-2-action")
                        .size(ButtonSize::Xs)
                        .variant(ButtonVariant::Outline)
                        .label("Connected"),
                ),
            );
        div()
            .grid()
            .grid_cols(2)
            .gap_x_8()
            .gap_y_5()
            .child(section("Table", cx, div().w_full().child(table)))
            .child(section("Discovery list", cx, discovery))
            .child(section(
                "Wordmark and icons",
                cx,
                row()
                    .gap_5()
                    .child(T3Wordmark::new().height(rems(1.5)).color(theme.foreground))
                    .child(MorphIcon::new(IconName::Copy).size(rems(1.)))
                    .child(
                        MorphIcon::new(IconName::Check)
                            .size(rems(1.))
                            .color(theme.success),
                    )
                    .child(RefreshIcon::new().size(RefreshIconSize::Sm))
                    .child(
                        RefreshIcon::new()
                            .size(RefreshIconSize::Lg)
                            .refreshing(true),
                    ),
            ))
            .child(section(
                "Start-truncated path",
                cx,
                div().w(px(220.)).child(StartTruncatedPath::new(
                    "path",
                    "/Users/jackson/Documents/Samwise/SM1/apps/web/src/components/ui/toast.tsx",
                )),
            ))
            .child(section(
                "Panel tab close button",
                cx,
                div()
                    .group(TAB_GROUP)
                    .flex()
                    .items_center()
                    .gap_2()
                    .child("Terminal 1")
                    .child(
                        PanelTabCloseButton::new(
                            "Terminal 1",
                            Icon::new(IconName::Code).size(rems(0.875)),
                        )
                        .tooltip("Close Terminal 1"),
                    ),
            ))
            .child(section(
                "Preview card",
                cx,
                div().h(px(120.)).pt(px(72.)).child(
                    PreviewCard::new(
                        Button::new("pc")
                            .variant(ButtonVariant::Outline)
                            .label("Hover target"),
                    )
                    .open(true)
                    .side(PreviewSide::Top)
                    .popup(div().w(px(220.)).p_3().child("Link preview content")),
                ),
            ))
            .child(section(
                "Animated height and width",
                cx,
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        Button::new("grow")
                            .size(ButtonSize::Xs)
                            .variant(ButtonVariant::Outline)
                            .label(if self.grow_content { "Shrink" } else { "Grow" })
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.grow_content = !this.grow_content;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .rounded(RADIUS_LG)
                            .border_1()
                            .border_color(theme.border)
                            .p_2()
                            .child(
                                AnimatedHeight::new("ah")
                                    .child(div().child("Measured content"))
                                    .when(self.grow_content, |this| {
                                        this.child(div().h(px(48.)).child("Extra row"))
                                    }),
                            ),
                    )
                    .child(
                        AnimatedWidth::new("aw", Some(if self.grow_content { 180. } else { 96. }))
                            .child(
                                div()
                                    .rounded(RADIUS_LG)
                                    .bg(with_alpha(theme.primary, 0.12))
                                    .px_2()
                                    .child("Width"),
                            ),
                    ),
            ))
            .into_any_element()
    }

    fn overlays(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let (year, month) = self.month;
        let selected = self.selected_day;
        let this = cx.entity();
        let month_this = cx.entity();
        let calendar = Calendar::new(year, month)
            .selected(selected)
            .today(NaiveDate::from_ymd_opt(2026, 10, 15).unwrap_or_default())
            .on_select(move |date, _, cx| {
                this.update(cx, |this, cx| {
                    this.selected_day = date;
                    cx.notify();
                });
            })
            .on_month(move |year, month, _, cx| {
                month_this.update(cx, |this, cx| {
                    this.month = (year, month);
                    cx.notify();
                });
            });
        let plane_this = cx.entity();
        let hue_this = cx.entity();
        let color = self.color;
        let picker = div()
            .w(px(240.))
            .rounded(RADIUS_LG)
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .overflow_hidden()
            .child(
                ColorSaturationValuePlane::new("plane", "Accent", color)
                    .variant(PlaneVariant::Edge)
                    .on_change(move |next, _, cx| {
                        plane_this.update(cx, |this, cx| {
                            this.color = next;
                            cx.notify();
                        });
                    }),
            )
            .child(div().px_3().py_1().child(
                ColorHueSlider::new("hue", "Accent hue", color.h).on_change(move |hue, _, cx| {
                    hue_this.update(cx, |this, cx| {
                        this.color.h = hue;
                        cx.notify();
                    });
                }),
            ));
        let open = self.collapsible_open;
        let collapsible = Collapsible::new("collapsible")
            .open(open)
            .trigger(
                Button::new("collapsible-trigger")
                    .size(ButtonSize::Xs)
                    .variant(ButtonVariant::Outline)
                    .icon(if open {
                        IconName::ChevronUp
                    } else {
                        IconName::ChevronDown
                    })
                    .label(if open { "Hide details" } else { "Show details" })
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.collapsible_open = !this.collapsible_open;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .pt_2()
                    .text_color(theme.muted_foreground)
                    .child("The panel travels its measured height over 200ms."),
            );
        let sheet_open = self.sheet_open;
        div()
            .grid()
            .grid_cols(3)
            .gap_x_6()
            .gap_y_5()
            .child(section("Calendar", cx, calendar))
            .child(section("Color picker", cx, picker))
            .child(section("Collapsible", cx, collapsible))
            .child(section(
                "Select ghost",
                cx,
                select_ghost_trigger("Auto", cx),
            ))
            .child(section(
                "Sheet",
                cx,
                Button::new("open-sheet")
                    .size(ButtonSize::Xs)
                    .variant(ButtonVariant::Outline)
                    .label("Open sheet")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.sheet_open = true;
                        cx.notify();
                    })),
            ))
            .when(sheet_open, |this| {
                let close = cx.entity();
                this.child(
                    Sheet::new("gallery-sheet", move |_, cx| {
                        close.update(cx, |this, cx| {
                            this.sheet_open = false;
                            cx.notify();
                        });
                    })
                    .side(SheetSide::Right)
                    .top_inset(px(0.))
                    .child(
                        sheet_header(false)
                            .child(sheet_title("Settings"))
                            .child(sheet_description("Edit gallery settings.", cx)),
                    ),
                )
            })
            .into_any_element()
    }

    fn popups(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let combobox_items = ["main", "feature/gallery", "fix/toast-swipe"];
        let combobox = div()
            .relative()
            .h(px(250.))
            .child(div().w(px(220.)).child(text_input_frame(cx).child("feat")))
            .child(
                ComboboxPopup::new()
                    .open(true)
                    .child(combobox_search_input("feat", cx))
                    .child(combobox_status("2 branches", cx))
                    .children(combobox_items.into_iter().enumerate().map(|(index, name)| {
                        ComboboxItem::new(name)
                            .selected(index == 0)
                            .highlighted(index == 1)
                    }))
                    .child(combobox_empty("No matches", cx)),
            );
        let autocomplete = div()
            .relative()
            .h(px(250.))
            .child(div().w(px(220.)).child(text_input_frame(cx).child("/re")))
            .child(
                AutocompletePopup::new()
                    .open(true)
                    .child(autocomplete_group_label("Commands", cx))
                    .child(AutocompleteItem::new("/review").highlighted(true))
                    .child(AutocompleteItem::new("/resume").selected(true)),
            );
        let dialog = div()
            .w(px(360.))
            .rounded(RADIUS_LG)
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .child(
                alert_dialog_header()
                    .child(alert_dialog_title("Delete thread?"))
                    .child(alert_dialog_description(
                        "This cannot be undone.",
                        window,
                        cx,
                    )),
            )
            .child(
                alert_dialog_footer(AlertDialogFooterVariant::Default, cx)
                    .child(
                        Button::new("ad-cancel")
                            .variant(ButtonVariant::Outline)
                            .label("Cancel"),
                    )
                    .child(
                        Button::new("ad-delete")
                            .variant(ButtonVariant::Destructive)
                            .label("Delete"),
                    ),
            );
        div()
            .grid()
            .grid_cols(3)
            .gap_x_6()
            .gap_y_5()
            .child(section("Combobox", cx, combobox))
            .child(section("Autocomplete", cx, autocomplete))
            .child(section(
                "Select ghost",
                cx,
                select_ghost_trigger("Auto", cx),
            ))
            .child(section("Alert dialog surface", cx, dialog))
            .into_any_element()
    }

    fn toasts_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        div()
            .flex()
            .flex_col()
            .gap_3()
            .text_color(theme.muted_foreground)
            .child("Success, stacked-action info, and a long error with a copy button.")
            .child("An anchored toast sits under the box at the left.")
            .into_any_element()
    }
}
