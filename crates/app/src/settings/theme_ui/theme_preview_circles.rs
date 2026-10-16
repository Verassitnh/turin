//! Port of upstream/apps/web/src/components/settings/ThemePreviewCircles.tsx
//!
//! A theme card's light and dark balls. Clicking a ball assigns that theme to
//! that half of the appearance mix; assigned balls carry a ring and a sun or
//! moon badge.
//!
//! GPUI has no radial gradients or blur, so each ball's glows are concentric
//! translucent discs (see PORTING.md, Deviations).

use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, CursorStyle, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px, relative, rems,
};

use crate::theme::color::{color, with_alpha};
use crate::theme::theme_palette::get_theme_colors_for_mode;
use crate::theme::theme_palette::get_theme_modes;
use crate::theme::theme_palettes::{ThemeAppearance, ThemeColors, ThemeDefinition};
use crate::theme::theme_preview::{
    ThemePreviewColors, mix_theme_preview_base, standard_theme_preview_colors,
    theme_preview_render_spec,
};
use crate::theme::tokens::{self, shadow_xs};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::{Icon, IconName, Tooltip};

/// `ThemeMode`: an appearance or "follow the system".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeMode {
    Light,
    Dark,
    System,
}

impl ThemeMode {
    pub fn appearance(self) -> Option<ThemeAppearance> {
        match self {
            Self::Light => Some(ThemeAppearance::Light),
            Self::Dark => Some(ThemeAppearance::Dark),
            Self::System => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
            Self::System => "system",
        }
    }
}

impl From<ThemeAppearance> for ThemeMode {
    fn from(appearance: ThemeAppearance) -> Self {
        match appearance {
            ThemeAppearance::Light => Self::Light,
            ThemeAppearance::Dark => Self::Dark,
        }
    }
}

/// `ThemeCardPreviewColors`: the seven roles a ball and a wireframe paint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeCardPreviewColors {
    pub sidebar: SharedString,
    pub canvas: SharedString,
    pub surface: SharedString,
    pub accent_surface: SharedString,
    pub accent: SharedString,
    pub message_surface: SharedString,
    pub message_action: SharedString,
}

impl ThemeCardPreviewColors {
    pub fn of(colors: &ThemeColors) -> Self {
        Self {
            sidebar: colors.sidebar.clone(),
            canvas: colors.canvas.clone(),
            surface: colors.surface.clone(),
            accent_surface: colors.accent_surface.clone(),
            accent: colors.accent.clone(),
            message_surface: colors.message_surface.clone(),
            message_action: colors.message_action.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeCardPreview {
    pub mode: ThemeAppearance,
    pub colors: ThemeCardPreviewColors,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeCardDefinition {
    pub id: String,
    pub label: String,
    pub previews: Vec<ThemeCardPreview>,
}

/// `STANDARD_THEME_PREVIEW_COLORS`.
fn standard_preview_colors(mode: ThemeAppearance) -> ThemeCardPreviewColors {
    let shared = standard_theme_preview_colors(mode);
    let (sidebar, surface, accent_surface, message_surface) = match mode {
        ThemeAppearance::Light => ("#fafafa", "#ffffff", "#f4f4f5", "#e4e4e7"),
        ThemeAppearance::Dark => ("#0f0f10", "#121212", "#27272a", "#27272a"),
    };
    ThemeCardPreviewColors {
        sidebar: sidebar.into(),
        canvas: shared.canvas.into(),
        surface: surface.into(),
        accent_surface: accent_surface.into(),
        accent: shared.accent.into(),
        message_surface: message_surface.into(),
        message_action: shared.message_action.into(),
    }
}

/// `STANDARD_THEME_CARDS`.
pub fn standard_theme_cards() -> Vec<ThemeCardDefinition> {
    vec![ThemeCardDefinition {
        id: "default".to_string(),
        label: "T3 Code".to_string(),
        previews: [ThemeAppearance::Light, ThemeAppearance::Dark]
            .into_iter()
            .map(|mode| ThemeCardPreview {
                mode,
                colors: standard_preview_colors(mode),
            })
            .collect(),
    }]
}

/// `previewColorsOf`.
pub fn preview_colors_of(
    card: &ThemeCardDefinition,
    mode: ThemeAppearance,
) -> Option<&ThemeCardPreviewColors> {
    card.previews
        .iter()
        .find(|preview| preview.mode == mode)
        .map(|preview| &preview.colors)
}

/// `getThemeCardDefinition`.
pub fn get_theme_card_definition(theme: &ThemeDefinition) -> ThemeCardDefinition {
    ThemeCardDefinition {
        id: theme.id.to_string(),
        label: theme.label.to_string(),
        previews: get_theme_modes(theme)
            .into_iter()
            .map(|mode| {
                let colors =
                    get_theme_colors_for_mode(theme, mode).unwrap_or_else(|| theme.colors.clone());
                ThemeCardPreview {
                    mode,
                    colors: ThemeCardPreviewColors::of(&colors),
                }
            })
            .collect(),
    }
}

fn c(value: &SharedString) -> Hsla {
    color(value.as_ref())
}

/// `ThemePreviewCircle`: a 56px ball (`border-2 border-background`) whose face
/// is the canvas mixed toward the mode base with an accent glow and a soft
/// action tint, plus the faint mode-appropriate inner ring that keeps its
/// silhouette legible (`themePreviewEdgeShadow`).
pub fn theme_preview_circle(
    colors: &ThemeCardPreviewColors,
    mode: ThemeAppearance,
    theme: &AppTheme,
) -> AnyElement {
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
    div()
        .relative()
        .size(px(56.))
        .flex_shrink_0()
        .overflow_hidden()
        .rounded_full()
        .border_2()
        .border_color(theme.background)
        .bg(color(&base))
        .child(disc(ax, ay, 0.33, with_alpha(accent, 0.30).into()))
        .child(disc(ax, ay, 0.24, with_alpha(accent, 0.55).into()))
        .child(disc(ax, ay, 0.13, with_alpha(accent, 0.85).into()))
        .child(disc(bx, by, 0.26, with_alpha(action, 0.16).into()))
        .child(disc(bx, by, 0.13, with_alpha(action, 0.28).into()))
        .child(
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

/// The ring and sun/moon badge an assigned ball carries.
fn picked_overlay(mode: ThemeAppearance, size: f32, theme: &AppTheme) -> [AnyElement; 2] {
    [
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .rounded_full()
            .border_2()
            .border_color(theme.ring)
            .into_any_element(),
        div()
            .absolute()
            .bottom(px(2.))
            .right(px(2.))
            .flex()
            .size(px(size))
            .items_center()
            .justify_center()
            .rounded_full()
            .border_1()
            .border_color(with_alpha(theme.border, 0.7))
            .bg(theme.background)
            .shadow(shadow_xs(0.1))
            .child(
                Icon::new(if mode == ThemeAppearance::Light {
                    IconName::Sun
                } else {
                    IconName::Moon
                })
                .size(rems(0.75))
                .color(theme.foreground),
            )
            .into_any_element(),
    ]
}

/// `ThemePreviewCircles`: one button per half; `on_select_mode` receives the
/// half that was clicked.
pub fn theme_preview_circles(
    id_prefix: &str,
    label: &str,
    active_modes: &[ThemeAppearance],
    previews: &[ThemeCardPreview],
    on_select_mode: Rc<dyn Fn(ThemeAppearance, &mut Window, &mut App)>,
    cx: &App,
) -> AnyElement {
    let theme = cx.app_theme().clone();
    div()
        .flex()
        .min_h_16()
        .items_center()
        .justify_center()
        .gap(rems(0.625))
        .px_3()
        .pt_3()
        .children(previews.iter().map(|preview| {
            let mode = preview.mode;
            let is_picked = active_modes.contains(&mode);
            let select = Rc::clone(&on_select_mode);
            let tooltip_text = if mode == ThemeAppearance::Light {
                "Use for light mode only"
            } else {
                "Use for dark mode only"
            };
            let [ring, badge] = picked_overlay(mode, 20., &theme);
            div()
                .id(SharedString::from(format!(
                    "{id_prefix}-ball-{}-{}",
                    label,
                    ThemeMode::from(mode).name()
                )))
                .relative()
                .flex()
                .size(px(68.))
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .rounded_full()
                .p_1()
                .cursor(CursorStyle::PointingHand)
                .tooltip(move |window, cx| Tooltip::new(tooltip_text).build(window, cx))
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    select(mode, window, cx)
                })
                .child(theme_preview_circle(&preview.colors, mode, &theme))
                .when(is_picked, |this| this.child(ring).child(badge))
        }))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::theme_palettes::{GROVE_THEME, T3_CHAT_THEME};

    #[test]
    fn the_standard_card_has_a_light_and_a_dark_ball() {
        let cards = standard_theme_cards();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].id, "default");
        assert_eq!(cards[0].label, "T3 Code");
        let modes: Vec<_> = cards[0].previews.iter().map(|p| p.mode).collect();
        assert_eq!(modes, [ThemeAppearance::Light, ThemeAppearance::Dark]);
        let light = preview_colors_of(&cards[0], ThemeAppearance::Light).expect("light");
        assert_eq!(light.sidebar.as_str(), "#fafafa");
        assert_eq!(light.canvas.as_str(), "#fcfcfc");
        let dark = preview_colors_of(&cards[0], ThemeAppearance::Dark).expect("dark");
        assert_eq!(dark.sidebar.as_str(), "#0f0f10");
        assert_eq!(dark.message_action.as_str(), "#8b9cff");
    }

    #[test]
    fn a_dual_mode_theme_card_has_both_previews_from_its_own_palettes() {
        let theme = T3_CHAT_THEME.clone();
        let card = get_theme_card_definition(&theme);
        assert_eq!(card.id, "t3-chat");
        assert_eq!(card.previews.len(), 2);
        let dark = theme.variants.dark.as_ref().expect("dark variant");
        let from_card = preview_colors_of(&card, ThemeAppearance::Dark).expect("dark");
        assert_eq!(from_card.canvas, dark.canvas);
        assert_eq!(from_card.message_action, dark.message_action);
    }

    #[test]
    fn a_single_appearance_theme_card_only_has_its_own_ball() {
        let single = ThemeDefinition {
            id: SharedString::new_static("solo"),
            appearance: ThemeAppearance::Dark,
            variants: crate::theme::theme_palettes::ThemeVariants {
                light: None,
                dark: None,
            },
            ..GROVE_THEME.clone()
        };
        let card = get_theme_card_definition(&single);
        assert_eq!(card.previews.len(), 1);
        assert_eq!(card.previews[0].mode, ThemeAppearance::Dark);
        assert_eq!(preview_colors_of(&card, ThemeAppearance::Light), None);
    }

    #[test]
    fn theme_mode_round_trips_with_appearance() {
        assert_eq!(ThemeMode::from(ThemeAppearance::Dark), ThemeMode::Dark);
        assert_eq!(ThemeMode::System.appearance(), None);
        assert_eq!(ThemeMode::Light.appearance(), Some(ThemeAppearance::Light));
        assert_eq!(ThemeMode::System.name(), "system");
    }
}
