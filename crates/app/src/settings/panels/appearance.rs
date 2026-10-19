//! Port of `AppearanceSettingsPanel` (SettingsPanels.tsx), ThemeSettings.tsx
//! (the built-in theme library), ThemePreviewCircles.tsx, ThemeWireframe.tsx
//! and the typography rows.
//!
//! "Create theme", the custom theme cards (Edit, Duplicate) and the built-in cards'
//! Duplicate open the theme editor (`theme_ui::theme_editor_host`, mounted by the
//! settings window).
//!
//! Not ported (PORTING.md Deviations): the import dialog ("Add theme") and community
//! theme search, removing a custom theme, the light/dark theme mix (`themeHalves`),
//! the live composer / diff / terminal font previews (static samples here) and
//! the terminal font row (terminal is cut).
//!
//! Port of upstream/apps/web/src/components/settings/ThemeSettings.tsx (built-in theme library).

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, CursorStyle, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    px, relative, rems,
};
use serde_json::json;

use super::super::controls::{FieldSize, SelectSize, SelectWidth, switch, text_field};
use super::super::layout::{SettingsRow, SettingsSection, setting_reset_button};
use super::super::rows::{BoolRow, Json};
use super::super::search::searchable_setting;
use super::super::slider::value_badge;
use super::super::view::{SettingsView, Which};
use crate::theme::color::{color, with_alpha};
use crate::theme::theme_editor_store::ThemeEditorSessionInput;
use crate::theme::theme_palette::{
    get_theme_colors_for_mode, get_theme_definition, get_theme_modes,
};
use crate::theme::theme_palette_library::get_custom_themes;
use crate::theme::theme_palettes::{
    BUILT_IN_THEMES, ThemeAppearance, ThemeColors, ThemeDefinition,
};
use crate::theme::theme_preview::{
    ThemePreviewColors, mix_theme_preview_base, standard_theme_preview_colors,
    theme_preview_render_spec,
};
use crate::theme::theme_runtime::{self, ThemeRuntime};
use crate::theme::tokens::{self, RADIUS_LG, RADIUS_XL, TEXT_SM, TEXT_XS};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName, Tooltip};

/// The seven roles `ThemeCardPreviewColors` carries.
#[derive(Clone)]
struct PreviewColors {
    sidebar: SharedString,
    canvas: SharedString,
    surface: SharedString,
    accent_surface: SharedString,
    accent: SharedString,
    message_surface: SharedString,
    message_action: SharedString,
}

fn preview_of(colors: &ThemeColors) -> PreviewColors {
    PreviewColors {
        sidebar: colors.sidebar.clone(),
        canvas: colors.canvas.clone(),
        surface: colors.surface.clone(),
        accent_surface: colors.accent_surface.clone(),
        accent: colors.accent.clone(),
        message_surface: colors.message_surface.clone(),
        message_action: colors.message_action.clone(),
    }
}

/// `STANDARD_THEME_PREVIEW_COLORS`.
fn standard_preview(mode: ThemeAppearance) -> PreviewColors {
    let shared = standard_theme_preview_colors(mode);
    match mode {
        ThemeAppearance::Light => PreviewColors {
            sidebar: "#fafafa".into(),
            surface: "#ffffff".into(),
            accent_surface: "#f4f4f5".into(),
            message_surface: "#e4e4e7".into(),
            canvas: shared.canvas.into(),
            accent: shared.accent.into(),
            message_action: shared.message_action.into(),
        },
        ThemeAppearance::Dark => PreviewColors {
            sidebar: "#0f0f10".into(),
            surface: "#121212".into(),
            accent_surface: "#27272a".into(),
            message_surface: "#27272a".into(),
            canvas: shared.canvas.into(),
            accent: shared.accent.into(),
            message_action: shared.message_action.into(),
        },
    }
}

struct ThemeCard {
    /// `None` is the standard "T3 Code" look.
    id: Option<SharedString>,
    label: SharedString,
    previews: Vec<(ThemeAppearance, PreviewColors)>,
    /// A saved theme: it can be edited as well as duplicated.
    custom: bool,
}

fn card_of(theme: &ThemeDefinition) -> ThemeCard {
    ThemeCard {
        id: Some(theme.id.clone()),
        label: theme.label.clone(),
        custom: false,
        previews: get_theme_modes(theme)
            .into_iter()
            .map(|mode| {
                let colors =
                    get_theme_colors_for_mode(theme, mode).unwrap_or_else(|| theme.colors.clone());
                (mode, preview_of(&colors))
            })
            .collect(),
    }
}

fn theme_cards() -> Vec<ThemeCard> {
    let mut cards = vec![ThemeCard {
        id: None,
        label: "T3 Code".into(),
        previews: [ThemeAppearance::Light, ThemeAppearance::Dark]
            .into_iter()
            .map(|mode| (mode, standard_preview(mode)))
            .collect(),
        custom: false,
    }];
    // Maintainer themes in upstream's order: T3 Chat, Grove, Ocean, Ember, Iris, then Cursor (Turin-only).
    cards.extend(BUILT_IN_THEMES.iter().map(card_of));
    // Then the saved themes, as `customThemeCollections` follows the maintainer ones.
    cards.extend(get_custom_themes().iter().map(|theme| ThemeCard {
        custom: true,
        ..card_of(theme)
    }));
    cards
}

fn c(value: &str) -> Hsla {
    color(value)
}

/// `ThemeWireframe` pane: a miniature of the app, all percentages of the frame.
fn wireframe_pane(colors: &PreviewColors) -> gpui_kit::Div {
    let line = with_alpha(gpui_kit::rgba(0x7f7f7f00), 0.25);
    let line: Hsla = Hsla::from(line);
    let region = |left: f32, top: f32, w: f32, h: f32, bg: Hsla| {
        div()
            .absolute()
            .left(relative(left))
            .top(relative(top))
            .w(relative(w))
            .h(relative(h))
            .bg(bg)
    };
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .bg(c(&colors.canvas))
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .w(relative(0.22))
                .bg(c(&colors.sidebar))
                .border_r_1()
                .border_color(line),
        )
        .child(
            region(0.03, 0.08, 0.16, 0.08, c(&colors.surface))
                .rounded_md()
                .border_1()
                .border_color(line),
        )
        .child(region(0.03, 0.22, 0.16, 0.07, c(&colors.accent_surface)).rounded_md())
        .child(
            region(0.03, 0.32, 0.16, 0.07, c(&colors.message_surface))
                .rounded_md()
                .opacity(0.7),
        )
        .child(
            region(0.03, 0.42, 0.16, 0.07, c(&colors.message_surface))
                .rounded_md()
                .opacity(0.5),
        )
        // Conversation.
        .child(region(0.48, 0.11, 0.24, 0.09, c(&colors.message_surface)).rounded_lg())
        .child(region(0.27, 0.28, 0.34, 0.05, line).rounded_sm())
        .child(region(0.27, 0.38, 0.26, 0.05, line).rounded_sm())
        // Composer.
        .child(
            region(0.26, 0.77, 0.68, 0.15, c(&colors.surface))
                .rounded_md()
                .border_1()
                .border_color(line)
                .flex()
                .items_center()
                .justify_between()
                .px_1()
                .child(
                    div()
                        .h(relative(0.26))
                        .w(relative(0.34))
                        .rounded_full()
                        .bg(line)
                        .opacity(0.7),
                )
                .child(
                    div()
                        .h(relative(0.58))
                        .w(px(10.))
                        .rounded_full()
                        .bg(c(&colors.message_action)),
                ),
        )
        // Orchestrator island.
        .child({
            let island = region(0.75, 0.08, 0.2, 0.46, c(&colors.surface))
                .rounded_lg()
                .border_1()
                .border_color(line);
            (0..3).fold(island, |island, row| {
                island.child(
                    div()
                        .absolute()
                        .left(relative(0.11))
                        .right(relative(0.11))
                        .top(relative(0.10 + row as f32 * 0.30))
                        .h(relative(0.20))
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .size(px(5.))
                                .rounded_full()
                                .opacity(0.55)
                                .bg(match row {
                                    0 => c("#34d399"),
                                    1 => c(&colors.message_action),
                                    _ => c("#fbbf24"),
                                }),
                        )
                        .child(
                            div()
                                .h(relative(0.3))
                                .w(relative(0.52))
                                .rounded_sm()
                                .bg(line),
                        ),
                )
            })
        })
}

fn wireframe(panes: Vec<(PreviewColors, Option<bool>)>, theme: &AppTheme) -> AnyElement {
    let frame = div()
        .relative()
        .w_full()
        .h(rems(8.75))
        .overflow_hidden()
        .rounded_lg()
        .border_1()
        .border_color(with_alpha(theme.border, 0.6));
    panes
        .into_iter()
        .fold(frame, |frame, (colors, clip)| match clip {
            None => frame.child(wireframe_pane(&colors)),
            // `clip: "left"` keeps the left half (`calc(50% - 1px)`).
            Some(left) => frame.child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .w(relative(0.5))
                    .overflow_hidden()
                    .when(left, |this| this.left_0())
                    .when(!left, |this| this.right_0())
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .w(relative(2.0))
                            .when(left, |this| this.left_0())
                            .when(!left, |this| this.right_0())
                            .child(wireframe_pane(&colors)),
                    ),
            ),
        })
        .into_any_element()
}

/// `ThemePreviewCircle`: a 56px ball (`border-2 border-background`) whose face
/// is the canvas mixed toward the mode base with an accent glow and a soft
/// action tint. GPUI has no radial gradients or blur, so the glows are
/// concentric translucent discs.
fn preview_circle(colors: &PreviewColors, mode: ThemeAppearance, theme: &AppTheme) -> AnyElement {
    let spec = theme_preview_render_spec(mode);
    let base = mix_theme_preview_base(
        &ThemePreviewColors {
            canvas: &colors.canvas,
            accent: &colors.accent,
            message_action: &colors.message_action,
        },
        mode,
    );
    let disc = |cx_frac: f32, cy_frac: f32, radius: f32, tint: Hsla| {
        div()
            .absolute()
            .left(relative(cx_frac - radius))
            .top(relative(cy_frac - radius))
            .size(relative(radius * 2.))
            .rounded_full()
            .bg(tint)
    };
    let accent = c(&colors.accent);
    let action = c(&colors.message_action);
    // GPUI clips to rectangles, so the glows are pulled toward the centre to
    // stay inside the circle.
    let pull = |(x, y): (f32, f32)| (0.5 + (x - 0.5) * 0.45, 0.5 + (y - 0.5) * 0.45);
    let (ax, ay) = pull(spec.accent.center);
    let (bx, by) = pull(spec.action.center);
    let edge = if mode == ThemeAppearance::Dark {
        with_alpha(tokens::tw::WHITE, 0.14)
    } else {
        with_alpha(tokens::tw::BLACK, 0.10)
    };
    let _ = theme;
    div()
        .relative()
        .size(px(56.))
        .flex_shrink_0()
        .overflow_hidden()
        .rounded_full()
        .border_2()
        .border_color(theme.background)
        .bg(c(&base))
        .child(disc(ax, ay, 0.33, with_alpha(accent, 0.30).into()))
        .child(disc(ax, ay, 0.24, with_alpha(accent, 0.55).into()))
        .child(disc(ax, ay, 0.13, with_alpha(accent, 0.85).into()))
        .child(disc(bx, by, 0.26, with_alpha(action, 0.16).into()))
        .child(disc(bx, by, 0.13, with_alpha(action, 0.28).into()))
        .child(
            // The faint inner ring that keeps a ball's silhouette legible.
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .rounded_full()
                .border_1()
                .border_color(edge),
        )
        .into_any_element()
}

/// `openThemeEditor`: a draft in the appearance on screen. Nothing happens where the theme
/// runtime is not installed (the demos).
fn open_theme_editor(
    editing_theme_id: Option<String>,
    seed_theme_id: Option<String>,
    seed_name: Option<String>,
    cx: &mut gpui_kit::App,
) {
    if !cx.has_global::<ThemeRuntime>() {
        return;
    }
    let initial_appearance = theme_runtime::snapshot(cx).resolved_theme;
    theme_runtime::update_editor(cx, |store| {
        store.open_theme_editor(ThemeEditorSessionInput {
            editing_theme_id,
            seed_theme_id,
            seed_name,
            initial_appearance,
        })
    });
}

/// "Create theme" starts from whatever is on screen for the appearance being edited, so
/// tuning the theme you already use never means rebuilding it.
fn create_theme(cx: &mut gpui_kit::App) {
    if !cx.has_global::<ThemeRuntime>() {
        return;
    }
    let snapshot = theme_runtime::snapshot(cx);
    let owner = snapshot
        .theme_halves
        .as_ref()
        .and_then(|halves| halves.get(snapshot.resolved_theme).map(str::to_owned))
        .or_else(|| get_theme_definition(&snapshot.theme).map(|theme| theme.id.to_string()));
    open_theme_editor(None, owner, None, cx);
}

impl SettingsView {
    pub(super) fn render_appearance(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let json = self.json(cx);
        let _ = window;
        vec![
            self.theme_library(cx),
            self.interface_section(&json, cx),
            self.motion_section(&json, cx),
            self.typography_section(&json, window, cx),
        ]
    }

    // ── Colors & themes ────────────────────────────────────────────────

    fn theme_library(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let vm = self.vm(cx);
        let mode = vm.appearance.mode.clone();
        let base_id: Option<SharedString> =
            get_theme_definition(&vm.appearance.theme.clone()).map(|d| d.id);
        let cards = theme_cards();

        // Mode tiles.
        let light_colors = standard_or_theme(base_id.as_ref(), ThemeAppearance::Light);
        let dark_colors = standard_or_theme(base_id.as_ref(), ThemeAppearance::Dark);
        let tiles = ["system", "light", "dark"].into_iter().map(|tile| {
            let active = mode == tile;
            let panes = match tile {
                "system" => vec![
                    (light_colors.clone(), Some(true)),
                    (dark_colors.clone(), Some(false)),
                ],
                "dark" => vec![(dark_colors.clone(), None)],
                _ => vec![(light_colors.clone(), None)],
            };
            let label = match tile {
                "system" => "System",
                "light" => "Light",
                _ => "Dark",
            };
            div()
                .id(SharedString::from(format!("appearance-mode-{tile}")))
                .flex()
                .flex_col()
                .items_stretch()
                .gap(rems(0.375))
                .flex_1()
                .p_2()
                .rounded(RADIUS_XL)
                .border_1()
                .cursor(CursorStyle::PointingHand)
                .when(active, |this| {
                    this.border_color(theme.ring)
                        .bg(with_alpha(theme.accent, 0.3))
                })
                .when(!active, |this| {
                    this.border_color(with_alpha(theme.border, 0.7))
                        .bg(with_alpha(theme.card, 0.6))
                        .hover(|style| style.bg(with_alpha(theme.accent, 0.1)))
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    let source = this.store.read(cx).source();
                    source.set_appearance_mode(tile.to_owned(), cx);
                }))
                .child(wireframe(panes, &theme))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(TEXT_XS.size)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(if active {
                            theme.foreground
                        } else {
                            theme.muted_foreground
                        })
                        .child(label),
                )
        });

        let heading = |text: &'static str| {
            div()
                .px_4()
                .text_size(TEXT_SM.size)
                .line_height(TEXT_SM.line_height)
                .text_color(with_alpha(theme.foreground, 0.7))
                .child(text)
        };

        // Card grid: `repeat(auto-fit, minmax(min(100%, 16rem), 1fr))`, gap-2.
        let row_width = cx
            .try_global::<super::super::layout::SettingsMetrics>()
            .map(|m| m.row_width + 32.)
            .unwrap_or(800.);
        let columns = (((row_width + 8.) / (256. + 8.)).floor() as usize).max(1);
        let card_width = (row_width - 8. * (columns as f32 - 1.)) / columns as f32;

        let grid = div()
            .flex()
            .flex_wrap()
            .gap_2()
            .children(cards.into_iter().map(|card| {
                let picked: Vec<ThemeAppearance> = [ThemeAppearance::Light, ThemeAppearance::Dark]
                    .into_iter()
                    .filter(|_| base_id == card.id)
                    .collect();
                let active = false;
                let id = card.id.clone();
                // Duplicate for every card, Edit for a saved theme. The card's own click
                // would pick the theme, so a press here stops there.
                let card_key = card.id.as_deref().unwrap_or("default").to_owned();
                let mut card_actions = Vec::new();
                if card.custom {
                    let (edit_id, edit_label) = (card.id.clone(), card.label.clone());
                    card_actions.push(
                        Button::new(SharedString::from(format!("theme-edit-{card_key}")))
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::IconXs)
                            .icon(IconName::Pencil)
                            .tooltip(Tooltip::new(format!("Edit {edit_label}")))
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                open_theme_editor(
                                    edit_id.as_ref().map(ToString::to_string),
                                    None,
                                    None,
                                    cx,
                                );
                            })
                            .into_any_element(),
                    );
                }
                {
                    let (duplicate_id, duplicate_label) = (card.id.clone(), card.label.clone());
                    card_actions.push(
                        Button::new(SharedString::from(format!("theme-duplicate-{card_key}")))
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::IconXs)
                            .icon(IconName::Copy)
                            .tooltip(Tooltip::new(format!("Duplicate {duplicate_label}")))
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                open_theme_editor(
                                    None,
                                    duplicate_id.as_ref().map(ToString::to_string),
                                    Some(format!("{duplicate_label} copy")),
                                    cx,
                                );
                            })
                            .into_any_element(),
                    );
                }
                let use_label = card.label;
                let current_mode = mode.clone();
                let use_card = cx.listener(move |this, _: &gpui_kit::ClickEvent, _, cx| {
                    let source = this.store.read(cx).source();
                    match id.clone() {
                        Some(id) => source.set_theme(id.to_string(), cx),
                        // The standard look: the preference is named after the mode.
                        None => source.set_theme(current_mode.clone(), cx),
                    }
                });
                let balls = card.previews.iter().map(|(preview_mode, colors)| {
                    let is_picked = picked.contains(preview_mode);
                    let ball = preview_circle(colors, *preview_mode, &theme);
                    div()
                        .relative()
                        .flex()
                        .size(px(68.))
                        .flex_shrink_0()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .p_1()
                        .child(ball)
                        .when(is_picked, |this| {
                            this.child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .left_0()
                                    .size_full()
                                    .rounded_full()
                                    .border_2()
                                    .border_color(theme.ring),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .bottom(px(2.))
                                    .right(px(2.))
                                    .flex()
                                    .size_5()
                                    .items_center()
                                    .justify_center()
                                    .rounded_full()
                                    .border_1()
                                    .border_color(with_alpha(theme.border, 0.7))
                                    .bg(theme.background)
                                    .shadow(tokens::shadow_xs(0.1))
                                    .child(
                                        Icon::new(if *preview_mode == ThemeAppearance::Light {
                                            IconName::Sun
                                        } else {
                                            IconName::Moon
                                        })
                                        .size(rems(0.75))
                                        .color(theme.foreground),
                                    ),
                            )
                        })
                });
                div()
                    .id(SharedString::from(format!(
                        "theme-card-{}",
                        card.id.as_deref().unwrap_or("default")
                    )))
                    .w(px(card_width))
                    .overflow_hidden()
                    .rounded(RADIUS_XL)
                    .border_1()
                    .cursor(CursorStyle::PointingHand)
                    .border_color(if active {
                        theme.ring
                    } else {
                        with_alpha(theme.border, 0.7).into()
                    })
                    .bg(if active {
                        with_alpha(theme.accent, 0.3)
                    } else {
                        with_alpha(theme.card, 0.6)
                    })
                    .hover(|style| style.bg(with_alpha(theme.accent, 0.1)))
                    .on_click(use_card)
                    .child(
                        div()
                            .flex()
                            .min_h_16()
                            .items_center()
                            .justify_center()
                            .gap(rems(0.625))
                            .px_3()
                            .pt_3()
                            .children(balls),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .pb_3()
                            .pt_2()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(TEXT_SM.size)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.foreground)
                                    .child(use_label),
                            )
                            .child(
                                div()
                                    .ml_auto()
                                    .flex()
                                    .items_center()
                                    .gap(px(2.))
                                    .children(card_actions),
                            ),
                    )
            }));

        div()
            .id("appearance")
            .flex()
            .flex_col()
            .gap_3()
            .child(heading(searchable_setting("color-scheme").1))
            .child(div().flex().gap_3().children(tiles))
            .child(
                div()
                    .flex()
                    .min_h_8()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px_0()
                    .pt_2()
                    .child(heading(searchable_setting("theme").1))
                    .child(
                        Button::new("create-theme")
                            .variant(ButtonVariant::Outline)
                            .size(ButtonSize::Xs)
                            .icon(IconName::Palette)
                            .label("Create theme")
                            .on_click(|_, _, cx| create_theme(cx)),
                    ),
            )
            .child(grid)
            .into_any_element()
    }

    // ── Interface ──────────────────────────────────────────────────────

    fn slider_row(
        &mut self,
        cx: &mut Context<Self>,
        json: &Json,
        search_id: &'static str,
        description: &'static str,
        key: &'static str,
        reset_label: &'static str,
        min: f64,
        max: f64,
        step: f64,
        suffix: &'static str,
        badge_width: f32,
        control_width: f32,
    ) -> AnyElement {
        let value = json.get(Which::Client, key).as_f64().unwrap_or(min);
        let slider = self.slider(cx, key, value, min, max, step, move |this, next, cx| {
            this.update_client(json!({ key: next as i64 }), cx);
        });
        let control = div()
            .flex()
            .w(rems(control_width))
            .items_center()
            .gap_3()
            .child(value_badge(
                format!("{}{suffix}", value as i64),
                badge_width,
                cx,
            ))
            .child(slider);
        Self::base_row(search_id)
            .description(description)
            .reset_action(self.reset_button(cx, json, Which::Client, key, reset_label))
            .control(control)
            .into_any_element()
    }

    fn interface_section(&mut self, json: &Json, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let contrast = self.slider_row(
            cx,
            json,
            "setting-appearance-contrast",
            "Adjust the contrast of colors and borders across the interface.",
            "appearanceContrast",
            "contrast",
            50.,
            200.,
            5.,
            "%",
            3.,
            13.,
        );
        let glass = self.slider_row(
            cx,
            json,
            "setting-glass-opacity",
            "Higher values make menus, dialogs, and the composer more solid.",
            "glassOpacity",
            "glass opacity",
            40.,
            100.,
            5.,
            "%",
            3.,
            13.,
        );

        // Diff colors: a select with the two diff dots in the trigger.
        let diff_scheme = json.str(Which::Client, "diffColorScheme").to_owned();
        let blue_orange = diff_scheme == "blue-orange";
        let dots = {
            let mut dots = vec![
                div()
                    .size(px(8.))
                    .rounded_full()
                    .bg(theme.diff_deletion_foreground),
                div()
                    .size(px(8.))
                    .rounded_full()
                    .bg(theme.diff_addition_foreground),
            ];
            if blue_orange {
                dots.reverse();
            }
            div().flex().flex_shrink_0().gap_1().children(dots)
        };
        let diff_control = self.select_control_rich(
            cx,
            "diff-color-scheme",
            div()
                .flex()
                .items_center()
                .gap(rems(0.5))
                .child(dots)
                .child(if blue_orange {
                    "Blue & orange"
                } else {
                    "Red & green"
                })
                .into_any_element(),
            vec![
                (
                    json!("red-green"),
                    div().child("Red & green (default)").into_any_element(),
                    !blue_orange,
                ),
                (
                    json!("blue-orange"),
                    div().child("Blue & orange").into_any_element(),
                    blue_orange,
                ),
            ],
            SelectWidth::Fixed(10.),
            |this, value, _, cx| this.update_client(json!({ "diffColorScheme": value }), cx),
        );
        let diff_row = Self::base_row("diff-color-scheme")
            .description("Choose colors for additions and deletions, including change counts.")
            .reset_action(self.reset_button(
                cx,
                json,
                Which::Client,
                "diffColorScheme",
                "diff colors",
            ))
            .control(diff_control)
            .into_any_element();

        let composer_context = self.bool_row(
            cx,
            json,
            BoolRow {
                search_id: "composer-context",
                title: None,
                description:
                    "Keep branch and worktree controls below the composer after a thread starts."
                        .into(),
                which: Which::Client,
                key: "persistComposerContextStrip",
                label: "Keep composer context visible in active threads",
                reset_label: "composer context",
            },
        );

        let chat_width = json.str(Which::Client, "chatWidth").to_owned();
        let chat_label = match chat_width.as_str() {
            "wide" => "Wide",
            "full" => "Full",
            _ => "Comfortable",
        };
        let chat_control = self.select_control_sized(
            cx,
            "chat-width",
            div().child(chat_label).into_any_element(),
            vec![
                (
                    json!("comfortable"),
                    div().child("Comfortable (default)").into_any_element(),
                    chat_width == "comfortable",
                ),
                (
                    json!("wide"),
                    div().child("Wide").into_any_element(),
                    chat_width == "wide",
                ),
                (
                    json!("full"),
                    div().child("Full").into_any_element(),
                    chat_width == "full",
                ),
            ],
            SelectWidth::Fixed(10.),
            SelectSize::Sm,
            false,
            |this, value, _, cx| this.update_client(json!({ "chatWidth": value }), cx),
        );
        let chat_row = Self::base_row("chat-width")
            .description("Set how wide messages and the composer can grow on large screens.")
            .reset_action(self.reset_button(cx, json, Which::Client, "chatWidth", "chat width"))
            .control(chat_control)
            .into_any_element();

        SettingsSection::new("Interface")
            .id("appearance-interface")
            .child(contrast)
            .child(glass)
            .child(diff_row)
            .child(composer_context)
            .child(chat_row)
            .into_any_element()
    }

    // ── Motion ─────────────────────────────────────────────────────────

    fn motion_section(&mut self, json: &Json, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let value = json
            .get(Which::Client, "panelAnimationDurationMs")
            .as_f64()
            .unwrap_or(0.);
        let slider = self.slider(
            cx,
            "panelAnimationDurationMs",
            value,
            0.,
            400.,
            25.,
            |this, next, cx| {
                this.update_client(json!({ "panelAnimationDurationMs": next as i64 }), cx);
            },
        );
        // `PanelAnimationsPreview`: a miniature window with sidebar, content and
        // an inspector panel (shown open; replay animation is not ported).
        let preview = div()
            .flex()
            .h_10()
            .w_full()
            .overflow_hidden()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .p_1()
            .shadow(tokens::shadow_xs(0.05))
            .child(
                div()
                    .h_full()
                    .w_4()
                    .flex_shrink_0()
                    .rounded_md()
                    .bg(theme.sidebar),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .flex_col()
                    .px_1()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .gap_1()
                            .pt_1()
                            .child(
                                div()
                                    .h(px(2.))
                                    .w_full()
                                    .rounded_full()
                                    .bg(with_alpha(theme.muted_foreground, 0.25)),
                            )
                            .child(
                                div()
                                    .h(px(2.))
                                    .w(relative(0.8))
                                    .rounded_full()
                                    .bg(with_alpha(theme.muted_foreground, 0.2)),
                            )
                            .child(
                                div()
                                    .h(px(2.))
                                    .w(relative(0.6))
                                    .rounded_full()
                                    .bg(with_alpha(theme.muted_foreground, 0.15)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .h_2()
                            .items_center()
                            .border_t_1()
                            .border_color(with_alpha(theme.border, 0.7))
                            .bg(with_alpha(theme.foreground, 0.05))
                            .px_2()
                            .child(
                                div()
                                    .h(px(1.))
                                    .w(relative(0.66))
                                    .rounded_full()
                                    .bg(with_alpha(theme.muted_foreground, 0.25)),
                            ),
                    ),
            )
            .child(
                div()
                    .h_full()
                    .w_5()
                    .flex_shrink_0()
                    .rounded_md()
                    .bg(theme.muted),
            );
        let control = div()
            .flex()
            .items_center()
            .gap_4()
            .child(div().w(rems(7.)).child(preview))
            .child(
                div()
                    .flex()
                    .w(rems(13.))
                    .items_center()
                    .gap_3()
                    .child(value_badge(format!("{} ms", value as i64), 4., cx))
                    .child(slider),
            );
        SettingsSection::new("Motion")
            .id("motion")
            .child(
                Self::base_row("panel-animations")
                    .description("Set how fast panels open and close.")
                    .control(control)
                    .reset_action(self.reset_button(
                        cx,
                        json,
                        Which::Client,
                        "panelAnimationDurationMs",
                        "panel animations",
                    )),
            )
            .into_any_element()
    }

    // ── Typography ─────────────────────────────────────────────────────

    fn typography_section(
        &mut self,
        json: &Json,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let advanced = self.panels.typography_advanced;
        let toggle = {
            let entity = cx.entity();
            switch(
                "switch-typography-advanced",
                advanced,
                false,
                cx,
                move |next, _, cx| {
                    entity.update(cx, |this, cx| {
                        this.panels.typography_advanced = next;
                        cx.notify();
                    });
                },
            )
        };
        let header_action = div()
            .flex()
            .items_center()
            .gap_2()
            .text_size(TEXT_XS.size)
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme.muted_foreground)
            .child("Advanced")
            .child(toggle);

        let mut section = SettingsSection::new("Typography")
            .id("typography")
            .header_action(header_action);
        if advanced {
            section = section
                .child(self.font_row(json, window, cx, FontRow::interface(false)))
                .child(self.font_row(json, window, cx, FontRow::prompt()))
                .child(self.font_row(json, window, cx, FontRow::code(false)));
            if self.vm(cx).is_mac() {
                section = section.child(
                    self.bool_row(
                        cx,
                        json,
                        BoolRow {
                            search_id: "font-smoothing",
                            title: None,
                            description:
                                "Use thinner grayscale text smoothing instead of the macOS default."
                                    .into(),
                            which: Which::Client,
                            key: "fontSmoothing",
                            label: "Font smoothing",
                            reset_label: "font smoothing",
                        },
                    ),
                );
            }
        } else {
            section = section
                .child(self.font_row(json, window, cx, FontRow::interface(true)))
                .child(self.font_row(json, window, cx, FontRow::code(true)));
        }
        section
            .child(self.bool_row(cx, json, BoolRow {
                search_id: "word-wrap",
                title: None,
                description: "Wrap long lines in code blocks, tables, diffs, and file previews by default.".into(),
                which: Which::Client,
                key: "wordWrap",
                label: "Wrap code, tables, diffs, and file previews by default",
                reset_label: "word wrapping",
            }))
            .into_any_element()
    }

    fn font_row(
        &mut self,
        json: &Json,
        window: &mut Window,
        cx: &mut Context<Self>,
        spec: FontRow,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let family = json.str(Which::Client, spec.family_key).to_owned();
        let size = json
            .get(Which::Client, spec.size_key)
            .as_i64()
            .unwrap_or(spec.min);
        let family_default = json.default_of(Which::Client, spec.family_key).clone();
        let size_default = json.default_of(Which::Client, spec.size_key).clone();
        let family_key = spec.family_key;
        let size_key = spec.size_key;

        let input = self.input(
            &format!("font-{family_key}"),
            &family,
            spec.placeholder,
            window,
            cx,
            move |this, text, _, cx| {
                this.update_client(json!({ family_key: text.trim() }), cx);
            },
        );
        let family_control =
            div()
                .w(rems(11.))
                .child(text_field(&input, None, FieldSize::Sm, false, cx));
        let size_control = self.select_control_sized(
            cx,
            &format!("font-size-{size_key}"),
            div().child(format!("{size} px")).into_any_element(),
            (spec.min..=spec.max)
                .map(|px_size| {
                    (
                        json!(px_size),
                        div().child(format!("{px_size} px")).into_any_element(),
                        px_size == size,
                    )
                })
                .collect(),
            SelectWidth::Fixed(5.5),
            SelectSize::Sm,
            false,
            move |this, value, _, cx| this.update_client(json!({ size_key: value }), cx),
        );
        let changed = json.get(Which::Client, family_key) != &family_default
            || json.get(Which::Client, size_key) != &size_default;
        let reset = changed.then(|| {
            let action = self.cb(cx, move |this, _, cx| {
                this.update_client(
                    json!({ family_key: family_default.clone(), size_key: size_default.clone() }),
                    cx,
                );
            });
            setting_reset_button(
                SharedString::from(format!("reset-font-{family_key}")),
                "Reset to default",
                move |w, c| action(w, c),
            )
            .into_any_element()
        });
        let control = div()
            .flex()
            .items_center()
            .gap_2()
            .child(family_control)
            .child(size_control);

        let preview = (spec.preview)(&theme, &family_text(json, &spec), size as f32);
        let (id, default_title) = searchable_setting(spec.search_id);
        SettingsRow::new(spec.title.unwrap_or(default_title))
            .id(id)
            .description(spec.description)
            .reset_action(reset)
            .control(control)
            .child(preview)
            .into_any_element()
    }
}

/// The family a preview paints with.
fn family_text(json: &Json, spec: &FontRow) -> String {
    json.str(Which::Client, spec.family_key).trim().to_owned()
}

struct FontRow {
    search_id: &'static str,
    title: Option<&'static str>,
    description: &'static str,
    family_key: &'static str,
    size_key: &'static str,
    placeholder: &'static str,
    min: i64,
    max: i64,
    preview: fn(&AppTheme, &str, f32) -> AnyElement,
}

impl FontRow {
    fn interface(with_prompt_preview: bool) -> Self {
        Self {
            search_id: "interface-font",
            title: None,
            description: "Everything outside code blocks and the terminal.",
            family_key: "fontFamilySans",
            size_key: "fontSizeInterface",
            placeholder: "System default",
            min: 12,
            max: 20,
            preview: if with_prompt_preview {
                prompt_preview
            } else {
                no_preview
            },
        }
    }

    fn prompt() -> Self {
        Self {
            search_id: "prompt-font",
            title: None,
            description: "Only the box you write prompts in. Mono works well here.",
            family_key: "fontFamilyComposer",
            size_key: "fontSizePrompt",
            placeholder: "System default",
            min: 12,
            max: 20,
            preview: prompt_preview,
        }
    }

    fn code(simple: bool) -> Self {
        Self {
            search_id: "code-font",
            title: simple.then_some("Monospace font"),
            description: if simple {
                "Code blocks, diffs, file previews, and the terminal."
            } else {
                "Code blocks, diffs, and file previews."
            },
            family_key: "fontFamilyCode",
            size_key: "fontSizeCode",
            placeholder: "System monospace",
            min: 10,
            max: 18,
            preview: code_preview,
        }
    }
}

fn no_preview(_: &AppTheme, _: &str, _: f32) -> AnyElement {
    div().into_any_element()
}

/// `PromptFontPreview`: the composer's text at the prompt family and size.
fn prompt_preview(theme: &AppTheme, family: &str, size: f32) -> AnyElement {
    div()
        .mt_1()
        .mb_2()
        .rounded(RADIUS_LG)
        .border_1()
        .border_color(theme.border)
        .bg(theme.background)
        .px_3()
        .py_2()
        .min_h(rems(3.5))
        .when(!family.is_empty(), |this| this.font_family(family.to_owned()))
        .text_size(px(size))
        .text_color(theme.foreground)
        .child("Use $frontend-design to fix the flaky test in surface.test.ts and align the header with SettingsPanels.tsx before shipping.")
        .into_any_element()
}

/// `CodeFontPreview`: a small unified diff at the code family and size.
fn code_preview(theme: &AppTheme, family: &str, size: f32) -> AnyElement {
    let mono = if family.is_empty() {
        theme.fonts.mono.clone()
    } else {
        family.to_owned()
    };
    let line = |prefix: &str, text: &str, bg: Option<Hsla>, fg: Hsla| {
        div()
            .flex()
            .px_3()
            .when_some(bg, |this, bg| this.bg(bg))
            .text_color(fg)
            .child(format!("{prefix} {text}"))
    };
    let add = theme.diff_addition_foreground;
    let del = theme.diff_deletion_foreground;
    div()
        .mt_1()
        .mb_2()
        .overflow_hidden()
        .rounded(RADIUS_LG)
        .border_1()
        .border_color(theme.border)
        .bg(theme.code_background)
        .py_2()
        .font_family(mono)
        .text_size(px(size))
        .text_color(theme.code_foreground)
        .child(line(
            " ",
            "export function formatUser(user: User) {",
            None,
            theme.code_foreground,
        ))
        .child(line(
            "-",
            "  return user.name.toUpperCase();",
            Some(with_alpha(del, 0.12).into()),
            theme.code_foreground,
        ))
        .child(line(
            "+",
            "  return `${user.name} <${user.email}>`; // 0O 1lI",
            Some(with_alpha(add, 0.12).into()),
            theme.code_foreground,
        ))
        .child(line(" ", "}", None, theme.code_foreground))
        .into_any_element()
}

fn standard_or_theme(base: Option<&SharedString>, mode: ThemeAppearance) -> PreviewColors {
    match base.and_then(|id| get_theme_definition(id)) {
        Some(definition) => {
            let definition = &definition;
            let colors = get_theme_colors_for_mode(definition, mode)
                .unwrap_or_else(|| definition.colors.clone());
            preview_of(&colors)
        }
        None => standard_preview(mode),
    }
}
